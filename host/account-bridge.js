// One-shot input to the original SDRT dialog. Never put credentials in ENV or URLs.
addToLibrary({
  // The guest now holds secrets (account input, persisted key containers): the page stops
  // exporting crash logs and memory states (web/src/play.ts).
  sake_mark_sensitive__sig: 'v',
  sake_mark_sensitive: () => {
    if (vnvmSensitive) Atomics.store(new Int32Array(vnvmSensitive, 0, 1), 0, 1);
  },
  sake_account_status__sig: 'vi',
  sake_account_status: (code) => {
    if (!vnvmAccount || code < 0 || code > 65535) return;
    Atomics.store(new Int32Array(vnvmAccount, 0, 1), 0, -code - 1);
  },
  // The dialog is the original's layout, but of a runtime build whose files are not the
  // verified ones: the input stays unused (4), and the page asks for it in the game.
  sake_account_unverified__sig: 'v',
  sake_account_unverified: () => {
    if (vnvmAccount) Atomics.compareExchange(new Int32Array(vnvmAccount, 0, 1), 0, 1, 4);
  },
  sake_account_take__sig: 'ipi',
  sake_account_take: (dst, cap) => {
    const shared = vnvmAccount;
    if (!shared || shared.byteLength !== 8 + 2 * (320 + 1024) || cap !== shared.byteLength - 4) return 0;
    const state = new Int32Array(shared, 0, 1);
    if (Atomics.compareExchange(state, 0, 1, 2) !== 1) return 0;
    const bytes = new Uint8Array(shared, 4);
    if (vnvmSensitive) Atomics.store(new Int32Array(vnvmSensitive, 0, 1), 0, 1);
    HEAPU8.set(bytes, dst >>> 0);
    bytes.fill(0);
    Atomics.store(state, 0, 3);
    return 1;
  },
});
