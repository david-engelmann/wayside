# Contract

Wayside asks whether the working state of an agent session can be recorded well enough that the session can be resumed, forked, or rewound after the process that ran it is gone. This file is the stub contract for the setup slice. It states the setup obligations only, and each obligation is stated so it can fail.

Obligation S1 covers seed integrity. The seed files README.md and LICENSE and .gitignore are present from the first commit, and every later change lands through a pull request rather than a direct push to main. This is covered by inspection of the repository history and by the status section of the README.

Obligation S2 covers the local gate. The test suite runs with cargo test plus the codepoint scan run through cargo, and running it needs no paid service, and the continuous integration workflow runs the same commands as the local gate. This wording is amended by the Rust port that opens Round 2. The setup wording named python and pytest alone and no network access, and the amendment replaces it under the program language ruling of 2026-11-02, which made every repo in the program a Rust crate. The amended gate keeps the no paid service term. It drops the no network access term for one stated reason, a first build fetches the crate dependencies from the public crates.io registry, and the tests themselves then run with no network access. This is covered by the tests in the tests folder and by the workflow in the .github folder.

Obligation S3 covers honest status. The README status section records the seed state and the contract state truthfully and it reports closed perfection rounds as 0 of 100 until a full cycle of work and audit and review and red team and approval has closed. This is covered by inspection of the README against this contract and against the repository history.

Obligation S4 covers synthetic content only. Every fixture and every example in this repository is synthetic and written for this repo, and no file carries a real trace or a real transcript or a real name or any private operational detail. This is covered by inspection of the repository contents before any push.

Obligation S5 covers the canonical agents file. The part of AGENTS.md above the marked line is identical to the canonical template for the program, and only the repo annex below the marked line differs for wayside. This is covered by tests in the tests folder that pin the SHA-256 digest of the part above the marked line and check the marked line and the annex.

The full contract for wayside is the first build act of this repo and it lands with the first slice. This stub is not the full contract, and no feature claim for wayside rests on this stub.

## Round 1 amendment and the full contract

Amendment, recorded with Round 1 on 2026-11-02. The closing paragraph of the setup stub above said the full contract lands with the first slice and that no feature claim rests on the stub. This section is that full contract landing. The stub text stays standing as the record of the setup state, and the setup obligations S1 to S5 stay in force beside the feature obligations below.

### Probe preamble

The obligations below rest on probes run against the real runtime on the build machine on 2026-11-02, before any of them was written.

A probe of flock ran 16 processes against one lock file, and each process took the exclusive lock 25 times to read and increment a shared counter and to write start and end markers around the increment. The counter finished at exactly 400 of 400 and the marker log held 800 lines with no interleaving, so on this machine an exclusive flock on one file is a real mutual exclusion across processes, and a sequence assigned while holding it cannot be assigned twice by racing writers.

A probe of rename assembled a directory of 50 files and renamed it into place 100 times while a polling reader watched for the new name, and in every trial the reader saw the directory complete with all 50 files the first time the name was visible and never saw it partial, so a checkpoint assembled under a temporary name and renamed inside the store is never visible half written. The same probe found that a rename from the probe scratch directory to /dev/shm and to the workspace directory failed with an invalid cross-device link error, so assembly and publication stay inside the store tree on one filesystem, and nothing in this slice publishes across filesystems.

A probe of hashlib found SHA-256 from the Python standard library matching the published test vectors, ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad for the three bytes abc and e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 for the empty input, under Python 3.12.3.

### Feature obligations

Obligation WS-1 covers what a checkpoint captures. A checkpoint records every regular file under the state directory the caller names, each by its relative path with its bytes, together with the env pairs the caller declares and the one memory document the caller supplies, and it records nothing else. The store never reads the live process environment, so a variable set in the calling process and not declared by the caller appears in no checkpoint. A state directory that does not exist or is not a directory, or one holding a symbolic link or a non regular file, or one holding a top level file named wayside.env or wayside.memory, which are the names restore writes, is refused with a clean error and nothing is recorded. This is covered by tests that checkpoint a synthetic state tree and read the manifest and the payloads back, and by a test that plants an undeclared variable in the process environment and checks the manifest for it.

Obligation WS-2 covers immutability and verified restore. The manifest of every checkpoint records the SHA-256 and the size of every payload file and of the memory document. Restore reads and verifies every recorded hash and size before it places a single byte in the target, and on any mismatch it fails with a corrupt checkpoint error and places nothing, so the target is left absent or empty exactly as it was found. A checkpoint that has been written is never modified by any operation in this slice, and restore and fork read payloads without changing them. This is covered by tests that flip one byte in a stored payload and in the stored memory document and check that restore fails and the target stays as it was, and by a test that reads a checkpoint directory back after later checkpoints and finds it unchanged.

Obligation WS-3 covers atomic publication. A checkpoint is assembled in a directory under a temporary name inside the store and renamed into its sequence name only when its manifest and payloads are complete, so a writer killed before the rename leaves no checkpoint that list or show can see. A directory under a temporary name inside the store is residue, and it is ignored by list and by show and by sequence assignment. This is covered by a test that kills a writer in the middle of a checkpoint and by a test that plants residue by hand and checks that list ignores it and the next sequence is unaffected.

Obligation WS-4 covers sequencing under one lock. Sequence numbers within a session start at 1 and rise by 1 with no gaps and no duplicates. The number is assigned while holding one exclusive flock on the store lock file, and the lock is held across reading the existing sequences and assigning the next number and writing the checkpoint and renaming it into place, so writers racing on one session cannot interleave or reuse a number. Fork takes the same lock while it creates a session. This is covered by a test that runs parallel writers against one session and checks that the published sequence is complete and gapless and that every manifest in it reads clean.

Obligation WS-5 covers safe names. A session id is one path safe component of lowercase letters and digits and hyphens and underscores, at most 64 characters long and beginning and ending with a letter or a digit, so a hyphen or an underscore never stands first or last, and anything else is refused with an invalid name error before the store is touched. Every file path a manifest records is a relative path with no absolute prefix and no parent reference and no empty component and no backslash, and restore validates every recorded path before placing anything, so a manifest damaged or written into naming a path outside the target is refused and nothing is placed. This is covered by tests with session ids and manifest paths carrying separators and parent references and absolute prefixes.

Obligation WS-6 covers strict manifest reading. A manifest is read strictly or not at all. A manifest that repeats a JSON key at any level, or names a format other than wayside-checkpoint/1, or carries a field set other than the recorded one at any level, or holds a value of the wrong type, or names a session or a sequence other than the directory it sits in, or is not valid UTF-8 or not valid JSON, is refused with a corrupt checkpoint error, and no default is substituted and no traceback escapes. This is covered by tests that damage a manifest in each of those ways and check the error that comes back.

Obligation WS-7 covers restore target discipline. Restore and rewind write only into a target directory that does not exist or exists and is empty, and they create it when it does not exist. A target that exists and is not empty, or exists and is not a directory, is refused and left byte for byte as it was found. This is covered by tests that restore into a fresh path and into an empty directory and against a non empty directory and against a file standing at the target path.

Obligation WS-8 covers fork lineage. Fork creates a new session whose first checkpoint carries content byte for byte equal to the named checkpoint of the source session, and the new manifest records the source session and the source sequence it was forked from. The source checkpoint is verified against its own recorded hashes before anything is copied, so a corrupt source is refused and no new session is created. Forking into a session that already holds a checkpoint is refused. This is covered by tests that fork and compare the restored content of both sessions and read the lineage back from the new manifest, and by a test that damages the source and checks that the fork is refused.

Obligation WS-9 covers rewind. Rewind restores a checkpoint of the session that stands earlier than the newest checkpoint of that session, into a target under the same discipline as restore. Rewinding to the newest sequence, or to a sequence the session does not hold, is refused with a clean error. This is covered by tests that checkpoint three states and rewind to the first and check the restored bytes, and by tests that refuse the newest sequence and a missing sequence.

Obligation WS-10 covers the named limits of this slice, and it fails if any public copy in this repo claims past them. The public copy under this cover is the README and the repo annex in AGENTS.md and this contract, each read against the code before any push. Slice 1 records files and declared env pairs and one memory document, and it does not snapshot database state, which the README names as part of the ambition of this repo and which enters in a later slice. Slice 1 has no cross-machine sync and no compression and no encryption and no daemon and no watcher, and a checkpoint is taken only when a caller asks for one. Slice 1 is not wired into the mayfly or switchyard runtimes, and it records state for any caller that names a store and a state directory. Slice 1 does not record empty directories in a state tree, only files and the directories that hold them. An env value is a single line of text, and a value carrying a newline or a NUL byte is refused at checkpoint time, because the env file restore writes carries one pair per line. Every failure this slice names is a typed error with a stable message and a nonzero exit from the command line, and the command line prints no traceback for any of them. This is covered by the error shape tests across the suite and by inspection of the README and the repo annex in AGENTS.md against this contract before any push.

## Round 1 repair note

Repair note, recorded with the Round 1 repair pass on 2026-11-02. The red team audit and the standards review of the first build of this slice found defects the build's own tests did not, and this note records what the repair changed, in the same way the amendment above recorded the full contract landing. The setup obligations S1 to S5 are untouched by this repair and stand byte for byte as written.

The store now refuses a symbolic link standing at or inside any component it relies on. A link at the sessions directory, or at a session directory, or at a checkpoints directory, or at a checkpoint directory, or at any entry inside a checkpoint, the manifest and the payloads included, is refused with a corrupt checkpoint error before any write and before any authoritative read, because the store's own writer creates only real directories and regular files, so a link inside the store is damage and never a shape the store made. Before this repair the store followed such links, and a planted link could redirect a checkpoint write outside the store or make list and show and restore treat outside bytes as a checkpoint of the store.

Manifest validation now refuses a recorded path that is exactly wayside.env or wayside.memory, closing the read side of the reservation WS-1 already enforced at checkpoint time, while a recorded path in a subdirectory carrying one of those names stays legal. Before this repair a manifest written into by hand could record a payload under one of those names and pass verification, and restore then wrote its own generated file over the verified payload and destroyed it without an error.

Sequence directory matching now accepts six or more digits, so a sequence past 999999 stays visible to list and to sequence assignment. Before this repair the listing pattern matched exactly six digits while the name format grew past them, so sequence 1000000 published and then stood invisible to list, and later checkpoints failed behind an error that blamed the lock.

The command line now refuses a memory document that is not a regular file before it reads anything, so a pipe named as the memory document fails with a clean error instead of hanging the command forever. The refusal covers a memory document named through a symbolic link as well as one that is not a regular file, and each shape fails with a message that names the shape it found.

A standing sequence directory whose manifest is missing is now corruption and not absence. List and show and restore raise a corrupt checkpoint error naming the session and the sequence and the missing manifest, where before this repair list failed with a not found error claiming the checkpoint did not stand. Sequence assignment keeps counting the standing directory, so the hole in the sequence stays as honest damage and the next checkpoint takes the number after it.

The repair also adds pins the obligations already claimed. The manifest damage pins now feed wrong typed values, a size as a string and env as a list and files as an object and a sequence as a string and a created_at that is not text or does not parse, under the coverage sentence of WS-6. The session id pins now cover the accepting edges of WS-5, exactly 64 characters and a single character and an id ending in a digit after hyphens and underscores, and the refusal of a trailing hyphen. A bad sequence at the command line now raises the invalid name error in both of its shapes, non numeric and non positive, so the same class of mistake exits with one code. The WS-5 sentence above is amended in this pass to state the beginning and ending rule the pattern already enforced, where it first stated only the character set and the length.

## Port amendment, the Rust conversion opening Round 2

Amendment, recorded with the Rust port that opens Round 2. The obligations WS-1 to WS-10 above stand unedited, and the implementation that carries them changes. The Python package of Round 1 is replaced by a Rust crate. The library carries the checkpoint store, the manifest reading and writing, the payload hashing, the sequence assignment under the store lock, and the six operations. The wayside binary carries the command line with the Python surface, the same subcommands and stdout lines and error lines and exit codes, and its argument parser mirrors the Python parser down to its usage texts and its parser error messages and its acceptance of unambiguous option prefixes. The codepoint scanner is ported from the fleet template in std only Rust. Obligation S2 is amended inside its own paragraph above, in the form the conversion of deadbolt set, with the program language ruling of 2026-11-02 named as its reason.

The equivalence standard for the port is threefold. The Round 1 test suite is translated pin for pin into the Rust suite. Five checkpoint stores written by the Python implementation stand frozen in the tests folder with their recorded list and show outputs and their recorded restore trees, and the Rust suite reads and restores and extends each one against those records. A fixed battery of command line shapes is driven side by side under the Python implementation from main and under the Rust binary, with exit codes and stdout and stderr compared shape by shape, and the comparison is recorded in the gate report of the port pull request. One pin beyond translation proves the manifest bytes, a checkpoint over a state tree holding a non ASCII file name and a non ASCII env value produces a manifest byte identical to the Python rendering for the same inputs.

The port adds a thin PyO3 binding over the crate as the round's second slice, landed separately after the port, because session checkpointing is called directly by agent frameworks that are often Python, while the port pull request itself carries no binding and the Rust suite remains the gate.

Six reshapings are named here, because the obligations and the observable behaviour are the spec and the shape of the Python code is not. The strict manifest reader is a hand written Visitor over serde_json that refuses a repeated key at any level as it arrives, because serde_json alone keeps the last value of a repeated key and says nothing, where the obligations require the refusal. Timestamps are parsed and written through chrono, and the reader accepts a family near the RFC 3339 family, narrower in stated places than the family the Python timestamp parser accepted. The reader takes a T separator or a space separator or a lowercase t in place of the T, with an uppercase Z or a numeric offset, and it requires the offset. It refuses a date alone and a value with no offset and a fraction written with a comma separator and a leap second count of 60 in the seconds field and a year of zero. It refuses a zone written with a lowercase z, exactly as the Python reader refused it. None of the refused shapes is one either implementation writes, so the narrowing can only meet a hand damaged manifest, where refusal is the safe direction. The manifest writer is hand rolled over the fixed shape of the format and reproduces the Python byte layout including its escaping of non ASCII text, because the general JSON writer in serde_json writes non ASCII text raw where the Python writer escapes it, and the stored bytes and the show output are both part of the recorded behaviour. The state tree walk lists directories through a small seam carried on a reader value, because the two walk error pins stage a listing failure and Rust has no patching of library calls and the suite runs as root where permission bits do not produce one, and the seam changes no default behaviour. The parallel writers pin and the killed writer pin are restaged on operating system processes and on the built binary, the shapes the languages offer for the same claims. The codepoint scanner adds the bin suffix to the binary suffixes it skips, one entry beyond the fleet scanner it ports, because this repo holds a recorded binary payload inside its checkpoint fixtures and the scan must pass its own tree, and a file with that suffix is binary by the same convention the rest of the list follows.

A further difference is recorded here as a safe direction divergence, found by the red team review of this port. A recorded size or a forked_from sequence wider than an unsigned 64 bit integer is refused at read time by this reader. The Python reader took such a shape at show, and where the number was a recorded size it refused only when a restore compared the size against the bytes it named, and where the number was a forked_from sequence it took the value without refusal, the lineage being informational. No manifest either implementation writes can carry such a number, because sizes are byte counts of payloads that stand on disk and sequences are assigned upward from 1, so the difference meets only a hand damaged manifest, and it refuses earlier rather than later.

The six named residuals of the Round 1 repair note are kept as named behaviour, none re-decided. The store side symlink refusal stands at the same points and under the same error. The reservation of the two generated file names stands on both the write side and the read side. Sequence directory matching past six digits stands. The refusal of a non regular memory document before any read stands in both its shapes. A sequence directory without a manifest stands as corruption that names the manifest, while sequence assignment keeps counting the standing directory. A directory in the state tree that cannot be listed stands as a failure naming that directory, staged through the seam named above.

## Round 4 amendment, the database snapshot slice

Amendment, recorded with Round 4 on 2026-11-05. The Round 1 amendment and the port
amendment above are the earlier record, and the obligations WS-1 to WS-10 stand beside the
obligations this amendment adds. One sentence of WS-10 is retired in the open by WS-19
below, the sentence saying slice 1 does not snapshot database state, and the rest of WS-10
stands. No probe preamble is written for this amendment, because the slice rests on the
Round 1 probes for rename publication and lock exclusion and on byte comparisons its own
pins perform, and on no other behaviour of a runtime.

Obligation WS-11 covers database declaration. At checkpoint time a caller may declare
database paths, each a relative path inside the state directory satisfying the recorded
path rules of WS-5 and naming a regular file that stands in the state directory. A path
declared twice, or a path that is the sidecar of another declared path, or a declared
path naming a top level file named wayside.env or wayside.memory, is refused with a clean
error before any byte is copied and before any sequence is assigned, and the store and
the sequence stand exactly as they stood. This is covered by tests that declare valid
paths and read the manifest back, and by tests for each refusal shape, a duplicate
declaration, a sidecar declared beside its main file, a reserved name, and a path with a
parent reference.

Obligation WS-12 covers the bundle and the opacity of database bytes. The bundle of a
declared path is the named file together with the file named by appending -wal and the
file named by appending -journal, when each stands as a regular file, and no other file.
The file named by appending -shm is never part of a bundle and is never recorded or
restored as one. No operation of this slice opens a database file or reads its format,
and a declared file of arbitrary bytes is captured, recorded, and restored exactly as a
declared file carrying a database header is. A sidecar standing in the state tree whose
main file is not declared is recorded as an ordinary file under WS-1. This is covered by
tests that checkpoint bundles with and without each sidecar standing, by a test that a
-shm file standing beside a declared database appears nowhere in the manifest or the
restored tree, and by a test that declares a file of patterned bytes with no database
structure and restores it byte for byte.

Obligation WS-13 covers the consistency proof at capture. Capture reads and hashes every
bundle member at the source, copies the members into the assembling checkpoint, and then
reads and hashes every member at the source again and re-checks which of the three
sidecar names stand. If any member's size or recorded hash differs between the two
readings, or a member stands at one reading and not the other, or a sidecar stands at
one reading and not the other, the checkpoint is refused with an error naming the
database path, and nothing is published and no sequence is consumed. The hashes the
manifest records are the hashes of the copied bytes. This is covered by tests that stage
a member changing between the readings through the state tree reader seam and check the
refusal and the untouched sequence, by tests that stage a sidecar appearing and a sidecar
disappearing between the readings, and by a test that a still bundle publishes with
manifest hashes equal to hashes computed from the source bytes.

Obligation WS-14 covers the bundle size bound. The members of one bundle may not sum
past 268435456 bytes. A bundle whose standing members sum past the bound is refused at
declaration validation, and a bundle that grows past the bound during its copy is
refused as the counted copy passes it, and in both cases the error names the bound and
nothing is published. No bound is added to ordinary file payloads, which keep the rule
of slice 1. This is covered by tests at the bound from both sides using sparse synthetic
members, and by a test that stages growth during the copy through the reader seam.

Obligation WS-15 covers the successor format. A checkpoint that declares at least one
database is written as wayside-checkpoint/2 and a checkpoint that declares none is
written as wayside-checkpoint/1, byte for byte as the landed writer writes it. The /2
manifest carries the /1 field set in the /1 order with one added field, databases,
standing between files and forked_from, and each database entry carries exactly the
fields path and bundle_sha256 and members, and each member carries exactly the fields
path and sha256 and size. Reading a /2 manifest refuses a repeated key at any level, a
field set other than the recorded one at any level, a value of the wrong type, an empty
databases array, a database or member path failing the rules of WS-5, member paths that
are not the database path followed by its -wal and -journal appendages in that order, and
any path recorded both as a database member and as an ordinary file, each as a corrupt
checkpoint error. Reading a /1 manifest behaves exactly as it did before this slice. This
is covered by tests that write both formats and read the manifests back field by field,
by a battery that damages a /2 manifest in each named way, and by the landed /1 fixture
stores standing readable and extendable in the same suite.

Obligation WS-16 covers restore and fork of database payloads. Restore and rewind of a
/2 checkpoint verify the hash and size of every database member before placing any byte,
under the rule of WS-2, and place the members at their recorded paths in the target under
the discipline of WS-7. Fork of a /2 checkpoint verifies the source first under WS-8 and
produces a /2 checkpoint whose database entries and bundle digests equal the source's.
This is covered by tests that flip one byte in a stored database member and check that
restore fails and the target stands as it was found, by tests that restore a bundle and
compare every placed byte against the source tree, and by a test that forks a /2
checkpoint and compares the digests and the restored trees of both sessions.

Obligation WS-17 covers verify. A verify operation names a session and a sequence and
reads the manifest strictly and re-reads and re-hashes every recorded payload, the files
and the memory document and every database member, and reports the checkpoint clean, or
fails with a corrupt checkpoint error naming the first payload whose bytes mismatch
their recorded hash and size. Verify writes nothing and creates nothing. This is covered
by tests that verify a clean checkpoint of each format, and by tests that damage a file
payload and a database member in the store and check that verify names the damaged
payload, and by a test that the store tree stands byte for byte unchanged after verify
runs.

Obligation WS-18 covers compare. A compare operation names a session and two sequences
and reads both manifests strictly and reads no payload bytes, and for every database path
recorded on either side it reports equal where both sides record the same bundle digest
for the path, and different where both record it with different digests, and recorded on
one side only where only one does, naming the side. A comparison where neither side
records any database reports no databases. Compare fails only where a manifest cannot be
read. This is covered by tests across two checkpoints whose bundles are byte identical
and byte different, by a test comparing a /1 checkpoint against a /2 checkpoint, and by a
test that compare runs against a store whose payloads were damaged after recording and
still reports the recorded digests, proving it read none of them.

Obligation WS-19 covers the limits of this slice, and it fails if any public copy in
this repo claims past them. This slice retires the WS-10 sentence that slice 1 does not
snapshot database state, and that retirement is recorded here rather than made by drift,
with the rest of WS-10 standing. The slice snapshots only declared file bundles inside a
state directory. It snapshots no database reached through a connection, no file outside
the state directory, and no state that lives only inside a running process, and it builds
no dump or load tooling. Its consistency proof shows a bundle sat still across one copy
by two readings that bracket the copy, and it does not freeze the database or lock its
writer, and a writer that restores a member's exact bytes inside the window is
indistinguishable from stillness. The slice adds no compression and no encryption and no
cross-machine sync and no daemon and no watcher, and it is not wired into the mayfly or
switchyard runtimes. Every failure this slice names is a typed error with a stable
message and a nonzero exit from the command line, and the command line prints no
traceback for any of them. This is covered by inspection of the README and the repo annex
in AGENTS.md against this contract before any push, and by the error shape tests across
the suite.

## Round 5 amendment, the empty directories slice

Amendment, recorded with Round 5 on 2026-11-09. The Round 1 amendment and the port amendment and the Round 4 amendment above are the earlier record, and the obligations WS-1 to WS-19 stand beside the obligations this amendment adds. One sentence of WS-10 is retired in the open by WS-24 below, the sentence saying slice 1 does not record empty directories in a state tree, and the rest of WS-10 stands. The writer and reader sentences of WS-15 are amended by the restatement named after the obligations below, and the rest of WS-15 stands. No probe preamble is written for this amendment, because the slice rests on the Round 1 probes for rename publication and lock exclusion and on byte comparisons its own pins perform, and on no other behaviour of a runtime.

Obligation WS-20 covers the directory record. A checkpoint records a directory standing in the state directory exactly when no recorded payload stands at or below it, where the recorded payloads are the checkpoint's file entries and the members of its database entries, so a directory holding only the -shm companion of a declared database is recorded and a directory holding any recorded file or member below it is not. The state directory itself is never recorded. Recorded directory paths satisfy the recorded path rules of WS-5, stand in path order with no path repeated, and stand prefix closed, so the ancestors of a recorded directory that also hold no recorded payload are recorded with it. A state tree holding a top level directory named wayside.env or wayside.memory is refused with a clean error before any byte is copied and before any sequence is assigned, under the reservation of WS-1 extended to directories. This is covered by tests that checkpoint trees with empty directories at several depths and read the records back, by a test that a directory holding a recorded payload below it is absent from the record, by a test staging the -shm only directory, and by tests for the reserved directory names standing empty and holding files.

Obligation WS-21 covers the amended format. A checkpoint is written as wayside-checkpoint/2 exactly when it declares at least one database or records at least one empty directory, and as wayside-checkpoint/1 otherwise, byte for byte as the writer wrote /1 before this amendment. The /2 manifest carries one further conditional field beside databases, named directories, whose value is an array of path strings standing in path order, and each of the two conditional fields stands present exactly when its content stands, databases when a database is declared and directories when an empty directory is recorded, and an empty array of either is corrupt. Reading a /2 manifest admits the two conditional fields and requires at least one of them, and refuses a repeated key at any level, a field set other than the recorded one at any level, a value of the wrong type, a directory path failing the rules of WS-5, a directory path repeated or out of order, a directory path standing also as a file or a database member, a directory path standing as a proper prefix of a recorded file or member path, and a top level directory path equal to a reserved name, each as a corrupt checkpoint error. Reading a /1 manifest behaves exactly as it did before this amendment, and a /1 manifest carrying a directories field is refused. A reader written before this amendment refuses a /2 manifest carrying directories under its field set check before placing a byte, and reads a /2 manifest carrying no directories exactly as it reads one written before this amendment. This is covered by tests that write each shape and read the manifests back field by field, by a battery that damages an amended manifest in each named way, by the frozen stores of the earlier formats standing readable and extendable in the same suite, and by a test that a checkpoint over a tree with no empty directories renders the manifest bytes the writer rendered before this amendment.

Obligation WS-22 covers restore and fork of directory records. Restore and rewind of a checkpoint verify the hash and size of every recorded payload before placing any byte, under the rule of WS-2, and then create each recorded directory in the target under the same directory creation discipline the store uses for the target itself and for the parents of placed payloads. Fork of a checkpoint carrying directory records produces a checkpoint whose directory records equal the source's, under the rule of WS-8. No mode and no ownership is recorded for a directory and none is restored. This is covered by tests that restore a checkpoint with empty directories and compare the restored tree's directory shape against the source tree, by a test that restores a chain of nested empty directories, and by a test that forks a checkpoint carrying directory records and compares the records and the restored trees of both sessions.

Obligation WS-23 covers what verify and compare vouch for after this amendment. Verify reads the manifest strictly and re-reads and re-hashes every recorded payload, the files and the memory document and every database member, as WS-17 states, and a clean verify vouches for the directory records as manifest shape under WS-21 and for no stored byte behind them, because the store holds no byte for a directory. The bundle digest of WS-15 is computed at capture from the copied bytes and at no other time, and verify does not recompute it from the member bytes, so a manifest whose bundle digest was edited inside the store verifies clean while compare, which reads the recorded digests, reports on the edited value, and the record states this bound rather than repairing it. Compare is unchanged by this amendment and reports nothing about directory records, and no recorded digest in either format covers a directory. This is covered by tests that verify a clean checkpoint carrying directory records, by the standing tests of WS-17 and WS-18 standing unedited beside it, and by a test that a bundle recorded in a tree with directories standing elsewhere carries the same bundle digest as the same bundle recorded without them.

Obligation WS-24 covers the limits of this slice, and it fails if any public copy in this repo claims past them. This slice retires the WS-10 sentence that slice 1 does not record empty directories in a state tree, and that retirement is recorded here rather than made by drift, with the rest of WS-10 standing. The slice records directories that hold no recorded payload and restores them as directories, and it records no directory mode and no directory ownership and no timestamp for any directory or any file. Its directory records live only in the manifest of wayside-checkpoint/2 as amended by WS-21, and no reader written before this amendment can read a manifest that carries them, though every such reader refuses it cleanly before placing a byte. The slice adds no compression and no encryption and no cross-machine sync and no daemon and no watcher, and it is not wired into the mayfly or switchyard runtimes. Every failure this slice names is a typed error with a stable message and a nonzero exit from the command line, and the command line prints no traceback for any of them. This is covered by inspection of the README and the repo annex in AGENTS.md against this contract before any push, and by the error shape tests across the suite.

One restatement is owed inside WS-15 itself, and the amendment makes it by naming it here rather than by silent edit. The WS-15 sentence that fixes when /2 is written gains the directories clause of WS-21, and its sentence requiring the databases field of every /2 reading is read from this amendment forward under the presence rule of WS-21. The rest of WS-15 stands. WS-1 gains no edit either, because WS-20 states the capture rule and the extended reservation as this amendment's own text, in the pattern WS-19 set when it retired a WS-10 sentence without editing WS-10 in place.
