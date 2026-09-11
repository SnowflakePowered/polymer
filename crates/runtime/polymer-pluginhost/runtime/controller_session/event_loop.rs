use super::{ControllerActor, deadline::IsolateDeadline};
use crate::runtime::js::ExtensionFunction;
use actix::{ActorContext, ActorFuture, ActorFutureExt, AtomicResponse, Context, fut};
use deno_core::{PollEventLoopOptions, v8};
use futures_util::{FutureExt, future::LocalBoxFuture};
use std::{
    future::Future,
    pin::Pin,
    task::{self, Poll},
    time::Duration,
};

/// Drives one callback while AtomicResponse holds the actor's mailbox.
/// The promise future owns its V8 handles but never borrows the runtime.
pub(super) struct DenoCall {
    promise: LocalBoxFuture<'static, Result<v8::Global<v8::Value>, Box<deno_core::error::JsError>>>,
    deadline: Option<IsolateDeadline>,
    timeout: Pin<Box<tokio::time::Sleep>>,
    timeout_error: String,
}

impl DenoCall {
    /// Preserve callback errors and mailbox ordering when no JS return value is needed.
    pub(super) fn completion(
        call: Result<Self, String>,
    ) -> AtomicResponse<ControllerActor, Result<(), String>> {
        match call {
            Ok(call) => AtomicResponse::new(Box::pin(call.map(|result, _, _| result.map(|_| ())))),
            Err(error) => AtomicResponse::new(Box::pin(fut::ready(Err(error)))),
        }
    }

    pub(super) fn start(
        actor: &mut ControllerActor,
        method: &str,
        arguments: &[v8::Global<v8::Value>],
        timeout: Duration,
        callback: &str,
    ) -> Result<Self, String> {
        if actor.control.is_cancelled() {
            return Err("controller plugin session is closed".to_owned());
        }
        let runtime = actor.runtime.js_runtime_mut();
        let function = ExtensionFunction::get(runtime, "__polymerEmulator", method)
            .map_err(|e| e.to_string())?;
        // Calling JS can block before it returns a promise, so arm the watchdog first.
        let deadline = IsolateDeadline::start(runtime.v8_isolate().thread_safe_handle(), timeout)
            .map_err(|e| e.to_string())?;
        let timer = Box::pin(tokio::time::sleep(timeout));
        let promise = function.start_call(runtime, arguments).boxed_local();
        Ok(Self {
            promise,
            deadline: Some(deadline),
            timeout: timer,
            timeout_error: format!("{callback} exceeded its {}ms deadline", timeout.as_millis()),
        })
    }

    fn poll_promise(
        &mut self,
        actor: &mut ControllerActor,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<v8::Global<v8::Value>, String>> {
        if let Poll::Ready(value) = self.promise.as_mut().poll(cx) {
            return Poll::Ready(value.map_err(|e| e.to_string()));
        }
        if let Poll::Ready(result) = actor
            .runtime
            .js_runtime_mut()
            .poll_event_loop(cx, PollEventLoopOptions::default())
        {
            if let Err(error) = result {
                return Poll::Ready(Err(error.to_string()));
            }
            return match self.promise.as_mut().poll(cx) {
                Poll::Ready(value) => Poll::Ready(value.map_err(|e| e.to_string())),
                Poll::Pending => Poll::Ready(Err(
                    "Promise resolution is still pending but the event loop has already resolved"
                        .to_owned(),
                )),
            };
        }
        Poll::Pending
    }
}

impl ActorFuture<ControllerActor> for DenoCall {
    type Output = Result<v8::Global<v8::Value>, String>;

    fn poll(
        self: Pin<&mut Self>,
        actor: &mut ControllerActor,
        ctx: &mut Context<ControllerActor>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Self::Output> {
        let this = self.get_mut();
        let result = if actor.control.is_cancelled() {
            ctx.stop();
            Poll::Ready(Err("controller plugin session is closed".to_owned()))
        } else if this.timeout.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(this.timeout_error.clone()))
        } else {
            this.poll_promise(actor, cx)
        };
        // Cancellation may arrive while poll_event_loop is executing JavaScript.
        if actor.control.is_cancelled() {
            ctx.stop();
        }
        let Poll::Ready(result) = result else {
            return Poll::Pending;
        };
        let expired = this
            .deadline
            .take()
            .expect("callback polled after completion")
            .finish(actor.runtime.js_runtime_mut());
        Poll::Ready(if expired {
            Err(this.timeout_error.clone())
        } else {
            result
        })
    }
}

/// Poll timers, asynchronous ops, and inspector traffic between messages too.
pub(super) struct IdleEventLoop;

impl ActorFuture<ControllerActor> for IdleEventLoop {
    type Output = ();

    fn poll(
        self: Pin<&mut Self>,
        actor: &mut ControllerActor,
        ctx: &mut Context<ControllerActor>,
        cx: &mut task::Context<'_>,
    ) -> Poll<()> {
        if actor.control.is_cancelled() {
            ctx.stop();
            return Poll::Ready(());
        }
        if let Poll::Ready(Err(error)) = actor
            .runtime
            .js_runtime_mut()
            .poll_event_loop(cx, PollEventLoopOptions::default())
        {
            tracing::error!(plugin = %actor.plugin.id(), "polymer: plugin event loop failed: {error}");
            ctx.stop();
            return Poll::Ready(());
        }
        Poll::Pending
    }
}
