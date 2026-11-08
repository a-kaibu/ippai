# Browser process filesystem sharing

Status: shared visibility, installation reload and gameplay (story, save/load) verified in Chromium.

## Evidence and requirements

The original SDRT runtime now completes its charging HTTP exchanges. In a native
filesystem, reopening the protected launcher without account input reaches the
game's Vorbis initialization. Before this change the browser launcher created MAJOKOI.exe, then CreateProcess
started an independent Wasm instance whose VFS could not see it. The shared VFS
now lets that original child executable initialize.
The vendor-created file must remain an ordinary live file; no pre-decrypted game
is added to the library, and the protected launcher remains the entry point.

Processes in one launch must observe the same files, directory changes and file
contents. Open handles retain their inode and seek position. Unlink must not
recycle an inode while another process holds it. Transactions must serialize
updates, including registry replacement and rename. Immutable archive data stays
in the streamed chunk store. Node's actual filesystem keeps its current behavior.

## Decision and alternatives

Use a per-launch shared memory region for the VFS namespace and file contents,
turned on by the first CreateProcess (crates/sake/src/kern/host.rs calls
`kura::vfs::share()` before the host starts the child): a single-process game
keeps its own file system and pays nothing for the sharing.
Each process keeps its handles locally; nodes have a shared open-reference count.
A lock covers importing the latest revision, a short in-memory operation, and
publishing changed state. Network chunk fetches and initial tree loading occur
outside this lock. Revision checks avoid copies for unchanged reads. The region is a
growable SharedArrayBuffer (1 MiB, up to 1 GiB; 64 MiB fixed where a browser
cannot grow one). A namespace past that maximum is not published: the writing
process goes on with its own files and logs it once, the others keep the last
namespace that fitted, rather than the game stopping. Open/close reference counts
publish the namespace through their own counter, not the saves' change count, so
opening files makes no save or installation export.

A snapshot only at spawn would fix the missing executable but would not share
later registry/service writes. Periodic merging introduces races and cannot make
rename or read-after-write reliable. A centralized filesystem worker could avoid
a fixed bound, but requires a new synchronous request protocol for every syscall;
it remains an alternative if snapshot size/copying is excessive.

## Risks and completion evidence

Validate parent-to-child and child-to-parent writes, concurrent writers, rename,
unlink with open handles, and handle offsets. Include inode reuse and malformed
snapshot tests. Main-thread exports must not fetch trees while holding a lock.
Snapshot costs (each change copies the namespace) remain a limitation of
multi-process runs. Shared files alone do not establish browser restart
persistence: installation state must be stored separately from portable game
saves, and generated process images must not enter portable exports. Those are
required follow-up implementation and tests before declaring browser play done.
No further user decision blocks implementation within the authorized goal.

## Validation so far

Fifteen Rust VFS tests pass, including preserving remote open inodes and local
seek offsets, and rejecting truncated shared snapshots without partial writes.
The host bridge test uses four worker threads and checks 1,200 serialized
read/modify/write operations plus capacity-failure behavior. Open/close reference
changes publish a new revision, so another process cannot recycle a live inode.
tests/vfs-shared.mjs also checks that sharing stays off until enabled and that the
growable buffer grows to the namespace and refuses past its maximum.
Process crashes can leave open references until the launch ends; crash cleanup
and snapshot-copy costs remain limitations of this implementation.

The actual browser launcher starts the vendor-created executable, which creates
its own visible window and submits Direct3D frames. Browser installation state
is saved separately with a non-extractable AES-GCM key; a page reload reaches
the game initialization without a new DMM authentication exchange. Portable
exports exclude provider keys, registry files and executable images. None of
these observations establishes correct visible gameplay or normal save/load.
