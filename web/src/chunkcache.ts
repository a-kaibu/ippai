/**
 * The size of kura's chunk cache (IPPAI_CHUNK_CACHE_MB, crates/kura/src/vfs.rs): the blocks of
 * the store's chunks a read decoded, kept in the wasm memory for the next read. The memory a
 * page of the wasm memory once held is never given back, so the whole cache stays in RAM for
 * the rest of the play. A device that says it has less than 8 GB (navigator.deviceMemory:
 * Chrome rounds it and caps it at 8; Android phones with 6 GB say 4) gets 32 MB: the game's
 * files are on the device by then (Cache Storage, the fetch ahead), so a block dropped is read
 * again from there. ドーナドーナ 体験版 in Chromium, title and 100 s of story: 32 MB took the page's
 * peak down 64 MB, and the game waited for files 4.84 s in all against 4.93 s at 128 MB (14
 * blocks read again). Others, and browsers that do not say: kura's default (128 MB).
 */
export function chunkCacheMb(deviceMemory: number | undefined): number | undefined {
  return deviceMemory !== undefined && deviceMemory > 0 && deviceMemory < 8 ? 32 : undefined;
}
