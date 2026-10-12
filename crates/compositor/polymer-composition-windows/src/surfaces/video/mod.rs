mod reader;
mod ring;

use polymer_composition::{VideoControl, VideoEvent, VideoSink, VideoSource};
use polymer_renderer::layout::Rect;
use polymer_renderer::{CompositingPlan, Extent, Renderer, SurfaceContent, SurfacePlacement};
use polymer_renderer_d3d12::D3d12Compositor;
use reader::{MediaReader, VideoFrame};
use ring::FrameRing;
use std::{
    error::Error,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Media::MediaFoundation::{MF_VERSION, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup},
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
    },
    core::IUnknown,
};

const IDLE_WAIT: Duration = Duration::from_millis(100);
const FRAME_WAIT: Duration = Duration::from_millis(10);
const AUDIO_LEAD: Duration = Duration::from_millis(200);
const POSITION_PERIOD: Duration = Duration::from_millis(250);

pub(crate) struct VideoSurface {
    content: IUnknown,
    controls: Sender<VideoControl>,
}

impl VideoSurface {
    pub fn spawn(
        id: u64,
        compositor: D3d12Compositor,
        source: VideoSource,
        sink: VideoSink,
    ) -> Result<Self, Box<dyn Error>> {
        let content = compositor.composition_content()?;
        let (controls, receiver) = mpsc::channel();
        thread::Builder::new()
            .name(format!("polymer-video-{id}"))
            .spawn(move || {
                let events = sink.events.clone();
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
                }
                let result =
                    VideoSession::open(id, compositor, &source, sink).and_then(|mut session| {
                        drop(source);
                        session.run(&receiver)
                    });
                if let Err(error) = result {
                    tracing::warn!(%error, "polymer: playing shell video {id} failed");
                    events(VideoEvent::Failed(error.to_string()));
                }
                unsafe {
                    let _ = MFShutdown();
                    CoUninitialize();
                }
            })?;
        Ok(Self { content, controls })
    }

    pub fn content(&self) -> IUnknown {
        self.content.clone()
    }

    pub fn control(&self, control: VideoControl) {
        let _ = self.controls.send(control);
    }
}

#[derive(Default)]
struct PlaybackClock {
    started: Option<Instant>,
    offset: Duration,
}

impl PlaybackClock {
    fn playing(&self) -> bool {
        self.started.is_some()
    }

    fn now(&self) -> Duration {
        self.offset
            + self
                .started
                .map_or(Duration::ZERO, |started| started.elapsed())
    }

    fn play(&mut self) {
        self.started.get_or_insert_with(Instant::now);
    }

    fn pause(&mut self) {
        self.offset = self.now();
        self.started = None;
    }

    fn set(&mut self, position: Duration) {
        self.offset = position;
        self.started = self.started.map(|_| Instant::now());
    }
}

struct VideoSession {
    id: u64,
    compositor: D3d12Compositor,
    reader: MediaReader,
    ring: FrameRing,
    sink: VideoSink,
    plan: CompositingPlan,
    clock: PlaybackClock,
    looping: bool,
    ended: bool,
    floor: Duration,
    pending: Option<VideoFrame>,
    video_done: bool,
    audio_done: bool,
    audio_until: Duration,
    audio_fresh: bool,
    presented: bool,
    reported: Instant,
}

impl VideoSession {
    fn open(
        id: u64,
        mut compositor: D3d12Compositor,
        source: &VideoSource,
        sink: VideoSink,
    ) -> Result<Self, Box<dyn Error>> {
        let reader = MediaReader::open(compositor.d3d12_device(), source)?;
        let (ring, plan) = Self::attach(id, &mut compositor, &reader)?;
        let session = Self {
            id,
            compositor,
            audio_done: reader.audio.is_none(),
            reader,
            ring,
            sink,
            plan,
            clock: PlaybackClock::default(),
            looping: false,
            ended: false,
            floor: Duration::ZERO,
            pending: None,
            video_done: false,
            audio_until: Duration::ZERO,
            audio_fresh: true,
            presented: false,
            reported: Instant::now(),
        };
        session.ready();
        Ok(session)
    }

    fn attach(
        id: u64,
        compositor: &mut D3d12Compositor,
        reader: &MediaReader,
    ) -> Result<(FrameRing, CompositingPlan), Box<dyn Error>> {
        let (width, height) = (reader.video.width, reader.video.height);
        compositor.resize_to(Extent::new(width, height))?;
        let (ring, stream) = FrameRing::new(compositor.d3d12_device(), &reader.video)?;
        compositor.import_local_stream(id, stream)?;
        let rectangle = Rect::new(0, 0, width, height);
        let mut placement = SurfacePlacement::new(id, id, rectangle, 0.0);
        placement.content = SurfaceContent::fill(rectangle);
        let plan = CompositingPlan::new([0.0, 0.0, 0.0, 1.0], vec![placement]);
        Ok((ring, plan))
    }

    fn ready(&self) {
        (self.sink.events)(VideoEvent::Ready {
            duration: self.reader.duration,
            width: self.reader.video.width,
            height: self.reader.video.height,
        });
    }

    fn run(&mut self, controls: &Receiver<VideoControl>) -> Result<(), Box<dyn Error>> {
        loop {
            self.advance()?;
            match controls.recv_timeout(self.wait()) {
                Ok(control) => self.apply(control)?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }

    fn wait(&self) -> Duration {
        match &self.pending {
            Some(frame) if self.clock.playing() => {
                frame.time.saturating_sub(self.clock.now()).min(FRAME_WAIT)
            }
            None if self.clock.playing() => FRAME_WAIT,
            _ => IDLE_WAIT,
        }
    }

    fn apply(&mut self, control: VideoControl) -> Result<(), Box<dyn Error>> {
        match control {
            VideoControl::Play if !self.clock.playing() => {
                if self.ended {
                    self.seek(Duration::ZERO)?;
                }
                self.clock.play();
                self.resume_audio();
            }
            VideoControl::Pause if self.clock.playing() => {
                self.clock.pause();
                self.sink.audio.pause(true);
            }
            VideoControl::Seek(position) => {
                self.seek(position)?;
                (self.sink.events)(VideoEvent::Seeked);
            }
            VideoControl::Loop(looping) => self.looping = looping,
            VideoControl::Play | VideoControl::Pause => {}
        }
        Ok(())
    }

    fn resume_audio(&mut self) {
        let Some(format) = self.reader.audio else {
            return;
        };
        if std::mem::take(&mut self.audio_fresh) {
            self.sink.audio.reset(format.sample_rate, format.channels);
        } else {
            self.sink.audio.pause(false);
        }
    }

    fn seek(&mut self, position: Duration) -> Result<(), Box<dyn Error>> {
        let position = self
            .reader
            .duration
            .map_or(position, |duration| position.min(duration));
        self.reader.seek(position)?;
        self.clock.set(position);
        self.floor = position;
        self.pending = None;
        self.ended = false;
        self.presented = false;
        self.video_done = false;
        self.audio_done = self.reader.audio.is_none();
        self.audio_until = position;
        self.audio_fresh = true;
        if self.clock.playing() {
            self.resume_audio();
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<(), Box<dyn Error>> {
        let now = self.clock.now();
        if self.clock.playing() {
            self.feed_audio(now)?;
        }
        if self.pending.is_none() && !self.video_done {
            self.pending = self.next_frame()?;
        }
        if self.clock.playing() && self.presented {
            self.skip_late(now)?;
        }
        if let Some(frame) = self
            .pending
            .take_if(|frame| !self.presented || frame.time <= now)
        {
            if let Some(ready) = self.ring.publish(&frame)? {
                Renderer::set_latest(&mut self.compositor, self.id, ready);
            }
            self.presented = true;
        }
        if Renderer::has_pending_frame(&self.compositor)
            && self.compositor.begin_frame_within(Duration::ZERO)
        {
            self.compositor.present_with_webrtc(&self.plan, None)?;
        }
        if self.clock.playing() {
            self.report(now)?;
        }
        Ok(())
    }

    fn read_frame(&mut self) -> Result<Option<VideoFrame>, Box<dyn Error>> {
        let frame = self.reader.read_video()?;
        if std::mem::take(&mut self.reader.reformatted) {
            (self.ring, self.plan) = Self::attach(self.id, &mut self.compositor, &self.reader)?;
            self.ready();
        }
        self.video_done = frame.is_none();
        Ok(frame)
    }

    fn next_frame(&mut self) -> Result<Option<VideoFrame>, Box<dyn Error>> {
        while let Some(frame) = self.read_frame()? {
            if frame.end > self.floor || frame.end == frame.time {
                return Ok(Some(frame));
            }
        }
        Ok(None)
    }

    fn skip_late(&mut self, now: Duration) -> Result<(), Box<dyn Error>> {
        while self
            .pending
            .as_ref()
            .is_some_and(|frame| frame.end > frame.time && frame.end <= now)
        {
            let Some(frame) = self.read_frame()? else {
                break;
            };
            self.pending = Some(frame);
        }
        Ok(())
    }

    fn feed_audio(&mut self, now: Duration) -> Result<(), Box<dyn Error>> {
        let Some(format) = self.reader.audio else {
            return Ok(());
        };
        while !self.audio_done && self.audio_until <= now + AUDIO_LEAD {
            let Some((time, samples)) = self.reader.read_audio()? else {
                self.audio_done = true;
                break;
            };
            let end = time + format.duration(samples.len());
            if end > self.floor {
                self.sink.audio.push(samples);
            }
            self.audio_until = self.audio_until.max(end);
        }
        Ok(())
    }

    fn report(&mut self, now: Duration) -> Result<(), Box<dyn Error>> {
        let finished = self.video_done
            && self.pending.is_none()
            && (self.audio_done && self.audio_until <= now);
        if finished && self.looping {
            self.seek(Duration::ZERO)?;
            (self.sink.events)(VideoEvent::Position(Duration::ZERO));
        } else if finished {
            self.clock.pause();
            self.ended = true;
            (self.sink.events)(VideoEvent::Ended);
        } else if self.reported.elapsed() >= POSITION_PERIOD {
            self.reported = Instant::now();
            (self.sink.events)(VideoEvent::Position(now));
        }
        Ok(())
    }
}
