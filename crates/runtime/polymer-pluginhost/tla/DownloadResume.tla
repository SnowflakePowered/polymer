--------------------------- MODULE DownloadResume ---------------------------
(***************************************************************************)
(* T13: resumable plugin download.                                         *)
(*                                                                         *)
(* Rust mapping (download/transfer.rs, download/mod.rs, polymer-library    *)
(* jobs/store.rs + jobs/handle.rs):                                        *)
(*   srv          the server's current copy of the URL (a version)         *)
(*   final        the cache file at Transfer::path; only written by the    *)
(*                rename of the `.part` file (Promote)                     *)
(*   cache        the `.part` file as the OS shows it (page cache; what    *)
(*                fs::metadata / File::open observe)                       *)
(*   disk         the bytes of the `.part` file that are durable on the    *)
(*                device; sync_all before every checkpoint save makes the  *)
(*                checkpointed prefix durable, OS writeback (Writeback)    *)
(*                may make more durable at any time                        *)
(*   ckpt         detail.checkpoint of the latest download job             *)
(*                (Checkpoint{etag, completed}); written by                *)
(*                Transfer::save via JobHandle::record (durable commit)    *)
(*   jobState     state of the latest download job (JobStore::latest)      *)
(*   prev,        `previous` job's detail and state read by run_as_job     *)
(*   prevState                                                             *)
(*   resume       the Option<Checkpoint> in Transfer::transfer             *)
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
(* Validators: "none" (no ETag), "weak" (W/ ETag per version), "strong"    *)
(* (ETag per version), "coarse" (ETag identical across versions).          *)
(* Only "strong" and "coarse" are resumed (Checkpoint::validator).         *)
(* Abstracted: max_byte_length, digest_length, progress reporting,         *)
(* concurrent requesters (enqueue dedupe / wait), url mismatch (one URL),  *)
(* durability of the rename itself.                                        *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Versions, Lengths, Expected, InitVersion, Interval,
          Check, Validators, BadRange, PowerLoss,
          MaxChanges, MaxCrashes, MaxNetFails

ASSUME Lengths \in [Versions -> Nat \ {0}]
ASSUME Expected \in Versions /\ InitVersion \in Versions
ASSUME Check \in {"none", "size", "digest"}
ASSUME Validators \in {"none", "weak", "strong", "coarse"}
ASSUME BadRange \in BOOLEAN /\ PowerLoss \in BOOLEAN

Garbage == <<0, 0>>
Body(v) == [i \in 1..Lengths[v] |-> <<v, i>>]
SingleVersion(f) == \E v \in Versions : f = Body(v)

NoCkpt == [val |-> -1, completed |-> -1]
NoResp == [ver |-> InitVersion, off |-> 0]

Val(v) == CASE Validators = "none"   -> 0
          []   Validators = "weak"   -> v
          []   Validators = "strong" -> v
          []   Validators = "coarse" -> 1

Resumable == Validators \in {"strong", "coarse"}

Checkable == Check # "none"

Verify(f) == CASE Check = "none"   -> TRUE
             []   Check = "size"   -> Len(f) = Lengths[Expected]
             []   Check = "digest" -> f = Body(Expected)

VARIABLES srv, changes, final, cache, disk, ckpt, jobState, prev, prevState,
          pc, phase, resumed, resume, resp, wck, saved, done,
          crashes, netfails

vars == <<srv, changes, final, cache, disk, ckpt, jobState, prev, prevState,
          pc, phase, resumed, resume, resp, wck, saved, done,
          crashes, netfails>>

MaxLen == CHOOSE m \in {Lengths[v] : v \in Versions} :
            \A v \in Versions : Lengths[v] <= m
Bytes == {Garbage} \cup {<<v, i>> : v \in Versions, i \in 1..MaxLen}
CkptT == {NoCkpt} \cup [val : {0} \cup Versions, completed : Nat]

TypeOK ==
  /\ srv \in Versions
  /\ final \in Seq(Bytes) /\ cache \in Seq(Bytes) /\ disk \in Seq(Bytes)
  /\ ckpt \in CkptT /\ prev \in CkptT /\ resume \in CkptT /\ wck \in CkptT
  /\ jobState \in {"none", "running", "completed", "failed"}
  /\ prevState \in {"none", "running", "completed", "failed"}
  /\ pc \in {"idle", "carry", "run", "request", "saveStart", "stream",
             "transferred", "promote", "ok", "fail"}
  /\ phase \in {1, 2} /\ resumed \in BOOLEAN /\ done \in BOOLEAN

Init ==
  /\ srv = InitVersion /\ changes = 0
  /\ final = <<>> /\ cache = <<>> /\ disk = <<>>
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
  /\ UNCHANGED <<srv, changes, final, cache, disk, phase, resumed, resume, resp,
                 wck, saved, done, crashes, netfails>>

\* run_as_job: handle.record(previous.detail) unless previous is Completed.
RecordPrevious ==
  /\ pc = "carry"
  /\ ckpt' = IF prevState # "completed" THEN prev ELSE ckpt
  /\ pc' = "run"
  /\ UNCHANGED <<srv, changes, final, cache, disk, jobState, prev, prevState,
                 phase, resumed, resume, resp, wck, saved, done, crashes,
                 netfails>>

\* Transfer::run up to the first transfer(resume).
Run ==
  /\ pc = "run"
  /\ IF Checkable /\ Verify(final)
       THEN /\ pc' = "ok"
            /\ UNCHANGED <<resume, resumed, phase>>
       ELSE LET r == IF Resumable /\ ckpt.completed > 0
                        /\ Len(cache) >= ckpt.completed
                       THEN ckpt ELSE NoCkpt
            IN /\ resume' = r /\ resumed' = (r # NoCkpt) /\ phase' = 1
               /\ pc' = "request"
  /\ UNCHANGED <<srv, changes, final, cache, disk, ckpt, jobState, prev,
                 prevState, resp, wck, saved, done, crashes, netfails>>

\* What the server may answer to GET with Range: bytes=r- and If-Range iv.
\* off is where the body really starts in the server's copy (Content-Range).
Responses(rs) ==
  LET L == Lengths[srv]
      r == rs.completed
      mismatch == rs.val # Val(srv)
  IN IF rs = NoCkpt THEN {[status |-> 200, off |-> 0]}
     ELSE IF mismatch THEN {[status |-> 200, off |-> 0]}
     ELSE (IF r < L THEN {[status |-> 206, off |-> r], [status |-> 200, off |-> 0]}
                    ELSE {[status |-> 416, off |-> 0]})
          \cup (IF BadRange
                  THEN {[status |-> 206, off |-> o] : o \in (0..(L - 1)) \ {r}}
                  ELSE {})

\* Checkpoint::rejected: a 416, or a 206 whose Content-Range does not start at
\* the checkpoint, makes Transfer::transfer resend without Range.
Rejected(rsp) ==
  rsp.status = 416 \/ (rsp.status = 206 /\ rsp.off # resume.completed)

\* Transfer::transfer: send, status match, set_len(start), seek(start).
TransferSend ==
  /\ pc = "request"
  /\ \E rsp \in Responses(resume) :
       IF resume # NoCkpt /\ Rejected(rsp)
         THEN /\ resume' = NoCkpt
              /\ UNCHANGED <<cache, resp, wck, pc>>
         ELSE LET start == IF rsp.status = 206 THEN resume.completed ELSE 0
              IN /\ cache' = SubSeq(cache, 1, start)
                 /\ wck' = [val |-> Val(srv), completed |-> start]
                 /\ resp' = [ver |-> srv, off |-> rsp.off]
                 /\ pc' = "saveStart"
                 /\ UNCHANGED resume
  /\ UNCHANGED <<srv, changes, final, disk, ckpt, jobState, prev, prevState,
                 phase, resumed, saved, done, crashes, netfails>>

\* Transfer::transfer: self.save(&checkpoint) before the first chunk.
SaveStart ==
  /\ pc = "saveStart"
  /\ ckpt' = wck /\ saved' = wck.completed
  /\ pc' = "stream"
  /\ UNCHANGED <<srv, changes, final, cache, disk, jobState, prev, prevState,
                 phase, resumed, resume, resp, wck, done, crashes, netfails>>

\* Transfer::transfer loop body: write_all(chunk); every Interval bytes
\* sync_all + save.
TransferChunk ==
  /\ pc = "stream"
  /\ resp.off < Lengths[resp.ver]
  /\ LET w == [wck EXCEPT !.completed = @ + 1]
         c == Append(cache, <<resp.ver, resp.off + 1>>)
     IN /\ cache' = c
        /\ resp' = [resp EXCEPT !.off = @ + 1]
        /\ wck' = w
        /\ IF w.completed - saved >= Interval
             THEN ckpt' = w /\ saved' = w.completed /\ disk' = c
             ELSE UNCHANGED <<ckpt, saved, disk>>
  /\ UNCHANGED <<srv, changes, final, jobState, prev, prevState, phase,
                 resumed, resume, pc, done, crashes, netfails>>

\* Transfer::transfer after the stream ends: sync_all + save.
TransferEnd ==
  /\ pc = "stream"
  /\ resp.off = Lengths[resp.ver]
  /\ ckpt' = wck /\ disk' = cache
  /\ pc' = "transferred"
  /\ UNCHANGED <<srv, changes, final, cache, jobState, prev, prevState, phase,
                 resumed, resume, resp, wck, saved, done, crashes, netfails>>

\* Transfer::run after a transfer returned Ok.
Verified ==
  /\ pc = "transferred"
  /\ IF phase = 1
       THEN IF ~Checkable \/ Verify(cache)
              THEN pc' = "promote" /\ UNCHANGED <<phase, resume>>
            ELSE IF resumed THEN /\ phase' = 2 /\ resume' = NoCkpt /\ pc' = "request"
            ELSE pc' = "fail" /\ UNCHANGED <<phase, resume>>
       ELSE /\ pc' = IF Verify(cache) THEN "promote" ELSE "fail"
            /\ UNCHANGED <<phase, resume>>
  /\ UNCHANGED <<srv, changes, final, cache, disk, ckpt, jobState, prev,
                 prevState, resumed, resp, wck, saved, done, crashes, netfails>>

\* Transfer::run: fs::rename(part, path).
Promote ==
  /\ pc = "promote"
  /\ final' = cache /\ cache' = <<>> /\ disk' = <<>>
  /\ pc' = "ok"
  /\ UNCHANGED <<srv, changes, ckpt, jobState, prev, prevState, phase,
                 resumed, resume, resp, wck, saved, done, crashes, netfails>>

\* run_as_job: handle.settle(result).
Settle ==
  /\ pc \in {"ok", "fail"}
  /\ jobState' = IF pc = "ok" THEN "completed" ELSE "failed"
  /\ done' = (pc = "ok")
  /\ pc' = "idle"
  /\ UNCHANGED <<srv, changes, final, cache, disk, ckpt, prev, prevState,
                 phase, resumed, resume, resp, wck, saved, crashes, netfails>>

Finished ==
  /\ pc = "idle" /\ done
  /\ UNCHANGED vars

\* A send() or chunk() error.
NetworkError ==
  /\ pc \in {"request", "stream"} /\ netfails < MaxNetFails
  /\ netfails' = netfails + 1
  /\ pc' = "fail"
  /\ UNCHANGED <<srv, changes, final, cache, disk, ckpt, jobState, prev,
                 prevState, phase, resumed, resume, resp, wck, saved, done,
                 crashes>>

ServerChange ==
  /\ changes < MaxChanges
  /\ changes' = changes + 1
  /\ srv' \in Versions \ {srv}
  /\ UNCHANGED <<final, cache, disk, ckpt, jobState, prev, prevState, pc,
                 phase, resumed, resume, resp, wck, saved, done, crashes,
                 netfails>>

Writeback ==
  /\ disk # cache
  /\ disk' = cache
  /\ UNCHANGED <<srv, changes, final, cache, ckpt, jobState, prev, prevState,
                 pc, phase, resumed, resume, resp, wck, saved, done, crashes,
                 netfails>>

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
  /\ UNCHANGED <<srv, changes, final, cache, disk, ckpt, done, netfails>>

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
  /\ UNCHANGED <<srv, changes, final, ckpt, done, netfails>>

Progress ==
  \/ RunAsJob \/ RecordPrevious \/ Run \/ TransferSend \/ SaveStart
  \/ TransferChunk \/ TransferEnd \/ Verified \/ Promote \/ Settle

Next ==
  \/ Progress \/ Finished
  \/ NetworkError \/ ServerChange \/ Writeback \/ ProcessCrash \/ PowerFailure

Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)

FinalIsSingleVersion == final = <<>> \/ SingleVersion(final)

CompletedIsSingleVersion == pc = "ok" => SingleVersion(final)

\* A checkpoint that Transfer::run would resume describes a durable prefix of
\* the version its validator names.
Prefix(f, c) == Len(f) >= c.completed /\ \A i \in 1..c.completed : f[i] = <<c.val, i>>
CheckpointIsDurable ==
  (Validators = "strong" /\ ckpt.completed > 0 /\ Len(cache) >= ckpt.completed)
    => Prefix(cache, ckpt) /\ Prefix(disk, ckpt)

\* Once the server settles on a copy the plugin accepts, the download succeeds.
EventuallyDownloaded == <>[]Verify(Body(srv)) => <>[]done
=============================================================================
