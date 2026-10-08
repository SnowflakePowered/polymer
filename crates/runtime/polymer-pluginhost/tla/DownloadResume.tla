--------------------------- MODULE DownloadResume ---------------------------
(***************************************************************************)
(* T13: resumable plugin download.                                         *)
(*                                                                         *)
(* Rust mapping (download/transfer.rs, download/mod.rs, polymer-library    *)
(* jobs/store.rs + jobs/handle.rs):                                        *)
(*   srv          the server's current copy of the URL (a version)         *)
(*   cache        the cache file at Transfer::path as the OS shows it      *)
(*                (page cache; what fs::metadata / File::open observe)     *)
(*   disk         the bytes of that file that are durable on the device;   *)
(*                File::flush is a no-op and nothing calls sync_all, so    *)
(*                only OS writeback (Writeback) makes bytes durable        *)
(*   ckpt         detail.checkpoint of the latest download job             *)
(*                (Checkpoint{etag|last_modified, completed}); written by  *)
(*                Transfer::save via JobHandle::record (durable commit)    *)
(*   jobState     state of the latest download job (JobStore::latest)      *)
(*   prev,        `previous` job's detail and state read by run_as_job     *)
(*   prevState                                                             *)
(*   resume       the Option<Checkpoint> passed to Transfer::transfer      *)
(*   resumed      Transfer::run `resumed`                                  *)
(*   phase        1 = first transfer in Transfer::run, 2 = the retry       *)
(*                transfer(None) after a resumed file failed verify()      *)
(*   resp         the response being streamed: which server version and   *)
(*                at which byte offset of it the body really starts        *)
(*   wck, saved   Transfer::transfer locals `checkpoint` and `saved`       *)
(*   done         the plugin's download() call has resolved Ok             *)
(*                                                                         *)
(* Bytes are <<version, index>>; Garbage is a byte no server version has.  *)
(* Check abstracts DownloadOptions: "none" (checkable() = false), "size"   *)
(* (byte_length only), "digest" (sha1/sha256 over the whole file).         *)
(* Validators: "none" (no ETag/Last-Modified), "strong" (ETag per          *)
(* version), "coarse" (Last-Modified identical across versions).           *)
(* Abstracted: max_byte_length, digest_length, progress reporting,         *)
(* concurrent requesters (enqueue dedupe / wait), url mismatch (one URL).  *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Versions, Lengths, Expected, InitVersion, Interval,
          Check, Validators, BadRange, PowerLoss,
          MaxChanges, MaxCrashes, MaxNetFails

ASSUME Lengths \in [Versions -> Nat \ {0}]
ASSUME Expected \in Versions /\ InitVersion \in Versions
ASSUME Check \in {"none", "size", "digest"}
ASSUME Validators \in {"none", "strong", "coarse"}
ASSUME BadRange \in BOOLEAN /\ PowerLoss \in BOOLEAN

Garbage == <<0, 0>>
Body(v) == [i \in 1..Lengths[v] |-> <<v, i>>]
SingleVersion(f) == \E v \in Versions : f = Body(v)

NoCkpt == [val |-> -1, completed |-> -1]
NoResp == [ver |-> InitVersion, off |-> 0]

Val(v) == CASE Validators = "none"   -> 0
          []   Validators = "strong" -> v
          []   Validators = "coarse" -> 1

Checkable == Check # "none"

Verify(f) == CASE Check = "none"   -> TRUE
             []   Check = "size"   -> Len(f) = Lengths[Expected]
             []   Check = "digest" -> f = Body(Expected)

VARIABLES srv, changes, cache, disk, ckpt, jobState, prev, prevState,
          pc, phase, resumed, resume, resp, wck, saved, done,
          crashes, netfails

vars == <<srv, changes, cache, disk, ckpt, jobState, prev, prevState,
          pc, phase, resumed, resume, resp, wck, saved, done,
          crashes, netfails>>

volatile == <<prev, prevState, phase, resumed, resume, resp, wck, saved>>

MaxLen == CHOOSE m \in {Lengths[v] : v \in Versions} :
            \A v \in Versions : Lengths[v] <= m
Bytes == {Garbage} \cup {<<v, i>> : v \in Versions, i \in 1..MaxLen}
CkptT == {NoCkpt} \cup [val : {0} \cup Versions, completed : Nat]

TypeOK ==
  /\ srv \in Versions
  /\ cache \in Seq(Bytes) /\ disk \in Seq(Bytes)
  /\ ckpt \in CkptT /\ prev \in CkptT /\ resume \in CkptT /\ wck \in CkptT
  /\ jobState \in {"none", "running", "completed", "failed"}
  /\ prevState \in {"none", "running", "completed", "failed"}
  /\ pc \in {"idle", "carry", "run", "request", "saveStart", "stream",
             "transferred", "ok", "fail"}
  /\ phase \in {1, 2} /\ resumed \in BOOLEAN /\ done \in BOOLEAN

Init ==
  /\ srv = InitVersion /\ changes = 0
  /\ cache = <<>> /\ disk = <<>>
  /\ ckpt = NoCkpt /\ jobState = "none"
  /\ prev = NoCkpt /\ prevState = "none"
  /\ pc = "idle" /\ phase = 1 /\ resumed = FALSE /\ resume = NoCkpt
  /\ resp = NoResp /\ wck = NoCkpt /\ saved = 0
  /\ done = FALSE /\ crashes = 0 /\ netfails = 0

\* run_as_job: store.latest + store.enqueue + store.claim (new job, empty detail).
RunAsJob ==
  /\ pc = "idle" /\ ~done
  /\ prev' = ckpt /\ prevState' = jobState
  /\ ckpt' = NoCkpt /\ jobState' = "running"
  /\ pc' = "carry"
  /\ UNCHANGED <<srv, changes, cache, disk, phase, resumed, resume, resp, wck,
                 saved, done, crashes, netfails>>

\* run_as_job: handle.record(previous.detail) unless previous is Completed.
RecordPrevious ==
  /\ pc = "carry"
  /\ ckpt' = IF prevState # "completed" THEN prev ELSE ckpt
  /\ pc' = "run"
  /\ UNCHANGED <<srv, changes, cache, disk, jobState, prev, prevState, phase,
                 resumed, resume, resp, wck, saved, done, crashes, netfails>>

\* Transfer::run up to the first transfer(resume).
Run ==
  /\ pc = "run"
  /\ IF Checkable /\ Verify(cache)
       THEN /\ pc' = "ok"
            /\ UNCHANGED <<resume, resumed, phase>>
       ELSE LET r == IF ckpt.completed > 0 /\ Len(cache) >= ckpt.completed
                       THEN ckpt ELSE NoCkpt
            IN /\ resume' = r /\ resumed' = (r # NoCkpt) /\ phase' = 1
               /\ pc' = "request"
  /\ UNCHANGED <<srv, changes, cache, disk, ckpt, jobState, prev, prevState,
                 resp, wck, saved, done, crashes, netfails>>

\* What the server may answer to GET with Range: bytes=r- and If-Range iv.
\* off is where the body really starts in the server's copy (Content-Range).
Responses(rs) ==
  LET L == Lengths[srv]
      r == rs.completed
      mismatch == rs.val # 0 /\ rs.val # Val(srv)
  IN IF rs = NoCkpt THEN {[status |-> 200, off |-> 0]}
     ELSE IF mismatch THEN {[status |-> 200, off |-> 0]}
     ELSE (IF r < L THEN {[status |-> 206, off |-> r], [status |-> 200, off |-> 0]}
                    ELSE {[status |-> 416, off |-> 0]})
          \cup (IF BadRange
                  THEN {[status |-> 206, off |-> o] : o \in (0..(L - 1)) \ {r}}
                  ELSE {})

\* Transfer::transfer: send, status match, set_len(start), seek(start).
TransferSend ==
  /\ pc = "request"
  /\ \E rsp \in Responses(resume) :
       IF rsp.status = 416 \/ (rsp.status = 206 /\ resume = NoCkpt)
         THEN /\ pc' = "fail"
              /\ UNCHANGED <<cache, resp, wck>>
         ELSE LET start == IF rsp.status = 206 THEN resume.completed ELSE 0
              IN /\ cache' = SubSeq(cache, 1, start)
                 /\ wck' = [val |-> Val(srv), completed |-> start]
                 /\ resp' = [ver |-> srv, off |-> rsp.off]
                 /\ pc' = "saveStart"
  /\ UNCHANGED <<srv, changes, disk, ckpt, jobState, prev, prevState, phase,
                 resumed, resume, saved, done, crashes, netfails>>

\* Transfer::transfer: self.save(&checkpoint) before the first chunk.
SaveStart ==
  /\ pc = "saveStart"
  /\ ckpt' = wck /\ saved' = wck.completed
  /\ pc' = "stream"
  /\ UNCHANGED <<srv, changes, cache, disk, jobState, prev, prevState, phase,
                 resumed, resume, resp, wck, done, crashes, netfails>>

\* Transfer::transfer loop body: write_all(chunk); every Interval bytes flush + save.
TransferChunk ==
  /\ pc = "stream"
  /\ resp.off < Lengths[resp.ver]
  /\ LET w == [wck EXCEPT !.completed = @ + 1]
     IN /\ cache' = Append(cache, <<resp.ver, resp.off + 1>>)
        /\ resp' = [resp EXCEPT !.off = @ + 1]
        /\ wck' = w
        /\ IF w.completed - saved >= Interval
             THEN ckpt' = w /\ saved' = w.completed
             ELSE UNCHANGED <<ckpt, saved>>
  /\ UNCHANGED <<srv, changes, disk, jobState, prev, prevState, phase, resumed,
                 resume, pc, done, crashes, netfails>>

\* Transfer::transfer after the stream ends: flush + save.
TransferEnd ==
  /\ pc = "stream"
  /\ resp.off = Lengths[resp.ver]
  /\ ckpt' = wck
  /\ pc' = "transferred"
  /\ UNCHANGED <<srv, changes, cache, disk, jobState, prev, prevState, phase,
                 resumed, resume, resp, wck, saved, done, crashes, netfails>>

\* Transfer::run after a transfer returned Ok.
Verified ==
  /\ pc = "transferred"
  /\ IF phase = 1
       THEN IF ~Checkable \/ Verify(cache) THEN pc' = "ok" /\ UNCHANGED <<phase, resume>>
            ELSE IF resumed THEN /\ phase' = 2 /\ resume' = NoCkpt /\ pc' = "request"
            ELSE pc' = "fail" /\ UNCHANGED <<phase, resume>>
       ELSE /\ pc' = IF Verify(cache) THEN "ok" ELSE "fail"
            /\ UNCHANGED <<phase, resume>>
  /\ UNCHANGED <<srv, changes, cache, disk, ckpt, jobState, prev, prevState,
                 resumed, resp, wck, saved, done, crashes, netfails>>

\* run_as_job: handle.settle(result).
Settle ==
  /\ pc \in {"ok", "fail"}
  /\ jobState' = IF pc = "ok" THEN "completed" ELSE "failed"
  /\ done' = (pc = "ok")
  /\ pc' = "idle"
  /\ UNCHANGED <<srv, changes, cache, disk, ckpt, prev, prevState, phase,
                 resumed, resume, resp, wck, saved, crashes, netfails>>

Finished ==
  /\ pc = "idle" /\ done
  /\ UNCHANGED vars

\* A send() or chunk() error.
NetworkError ==
  /\ pc \in {"request", "stream"} /\ netfails < MaxNetFails
  /\ netfails' = netfails + 1
  /\ pc' = "fail"
  /\ UNCHANGED <<srv, changes, cache, disk, ckpt, jobState, prev, prevState,
                 phase, resumed, resume, resp, wck, saved, done, crashes>>

ServerChange ==
  /\ changes < MaxChanges
  /\ changes' = changes + 1
  /\ srv' \in Versions \ {srv}
  /\ UNCHANGED <<cache, disk, ckpt, jobState, prev, prevState, pc, phase,
                 resumed, resume, resp, wck, saved, done, crashes, netfails>>

Writeback ==
  /\ disk # cache
  /\ disk' = cache
  /\ UNCHANGED <<srv, changes, cache, ckpt, jobState, prev, prevState, pc,
                 phase, resumed, resume, resp, wck, saved, done, crashes, netfails>>

\* Restart: JobStore::abandon(Download) fails the running job.
Restart ==
  /\ jobState' = IF jobState = "running" THEN "failed" ELSE jobState
  /\ pc' = "idle"
  /\ prev' = NoCkpt /\ prevState' = "none" /\ phase' = 1 /\ resumed' = FALSE
  /\ resume' = NoCkpt /\ resp' = NoResp /\ wck' = NoCkpt /\ saved' = 0
  /\ crashes' = crashes + 1

ProcessCrash ==
  /\ crashes < MaxCrashes
  /\ Restart
  /\ UNCHANGED <<srv, changes, cache, disk, ckpt, done, netfails>>

\* The file reverts to its durable bytes; its length may have been persisted
\* ahead of its data, leaving unwritten tail bytes.
AfterPowerLoss ==
  {disk} \cup
  {[i \in 1..Len(cache) |-> IF i <= Len(disk) THEN disk[i] ELSE Garbage]}

PowerFailure ==
  /\ PowerLoss /\ crashes < MaxCrashes
  /\ cache' \in AfterPowerLoss
  /\ disk' = cache'
  /\ Restart
  /\ UNCHANGED <<srv, changes, ckpt, done, netfails>>

Progress ==
  \/ RunAsJob \/ RecordPrevious \/ Run \/ TransferSend \/ SaveStart
  \/ TransferChunk \/ TransferEnd \/ Verified \/ Settle

Next ==
  \/ Progress \/ Finished
  \/ NetworkError \/ ServerChange \/ Writeback \/ ProcessCrash \/ PowerFailure

Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)

CompletedIsSingleVersion == pc = "ok" => SingleVersion(cache)

EventuallyDownloaded == <>[]done
=============================================================================
