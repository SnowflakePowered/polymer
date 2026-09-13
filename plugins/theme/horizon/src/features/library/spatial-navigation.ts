export type Direction = "left" | "right" | "up" | "down";
export type PositionedItem = { id: string; rect: DOMRect };

export function nearestInDirection(items: PositionedItem[], currentId: string, direction: Direction) {
  const current = items.find((item) => item.id === currentId);
  if (!current) return null;
  const cx = current.rect.left + current.rect.width / 2;
  const cy = current.rect.top + current.rect.height / 2;
  let winner: { id: string; score: number } | null = null;

  for (const item of items) {
    if (item.id === currentId) continue;
    const x = item.rect.left + item.rect.width / 2;
    const y = item.rect.top + item.rect.height / 2;
    const dx = x - cx;
    const dy = y - cy;
    const primary = direction === "left" ? -dx : direction === "right" ? dx : direction === "up" ? -dy : dy;
    if (primary <= 2) continue;
    const secondary = direction === "left" || direction === "right" ? Math.abs(dy) : Math.abs(dx);
    const score = primary + secondary * 2.25;
    if (!winner || score < winner.score) winner = { id: item.id, score };
  }
  return winner?.id || null;
}

export function gameId(game: { path: string; emulator: string }) {
  return `${game.path}\u0000${game.emulator}`;
}

function hash(value: string) {
  let result = 2166136261;
  for (let index = 0; index < value.length; index++) {
    result ^= value.charCodeAt(index);
    result = Math.imul(result, 16777619);
  }
  return result >>> 0;
}

export function tileColor(id: string) {
  const colors = ["#25334a", "#593035", "#27483d", "#564327", "#34305b", "#324b56", "#532b51"];
  return colors[hash(id) % colors.length];
}
