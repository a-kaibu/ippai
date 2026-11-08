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

Use a per-launch shared memory region for the VFS namespace and file contents.
Each process keeps its handles locally; nodes have a shared open-reference count.
A lock covers importing the latest revision, a short in-memory operation, and
publishing changed state. Network chunk fetches and initial tree loading occur
outside this lock. Revision checks avoid copies for unchanged reads. Start with
64 MiB of shared-state capacity; exceeding it must stop explicitly rather than
silently acknowledge a lost update. Measure serialized state size and write cost
with the title before accepting this bound for release.

A snapshot only at spawn would fix the missing executable but would not share
later registry/service writes. Periodic merging introduces races and cannot make
rename or read-after-write reliable. A centralized filesystem worker could avoid
a fixed bound, but requires a new synchronous request protocol for every syscall;
it remains an alternative if snapshot size/copying is excessive.

## Risks and completion evidence

Validate parent-to-child and child-to-parent writes, concurrent writers, rename,
unlink with open handles, and handle offsets. Include inode reuse and malformed
snapshot tests. Main-thread exports must not fetch trees while holding a lock.
The 64 MiB limit and snapshot costs are accepted development constraints, not yet
validated release limits. Shared files alone do not establish browser restart
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
Process crashes can leave open references until the launch ends; crash cleanup
and snapshot-copy costs remain limitations of this implementation.

The actual browser launcher starts the vendor-created executable, which creates
its own visible window and submits Direct3D frames. Browser installation state
is saved separately with a non-extractable AES-GCM key; a page reload reaches
the game initialization without a new DMM authentication exchange. Portable
exports exclude provider keys, registry files and executable images. None of
these observations establishes correct visible gameplay or normal save/load.
