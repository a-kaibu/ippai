// Stable, disjoint round-robin shards after the caller's normal probe filters.
export function partition(names, shard = '0/1') {
  const match = /^(\d+)\/([1-9]\d*)$/.exec(shard);
  if (!match || Number(match[1]) >= Number(match[2])) throw new Error(`invalid SHARD: ${shard}`);
  const [index, count] = match.slice(1).map(Number);
  return names.filter((_, i) => i % count === index);
}
