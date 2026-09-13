export type CoverPlacement = { x: number; y: number; width: number; height: number };
/** Justified rows preserve every cover's ratio and share an exact right edge. */
export class CoverLayout {
  static arrange(ratios: number[], width: number, targetHeight: number) {
    const placements: CoverPlacement[] = [];
    let y = 0;
    let start = 0;
    while (start < ratios.length) {
      let end = start, sum = 0;
      while (end < ratios.length && sum * targetHeight < width) sum += ratios[end++];
      const previous = sum - ratios[end - 1];
      if (end - start > 1 && Math.abs(width / previous - targetHeight) < Math.abs(width / sum - targetHeight)) {
        sum = previous;
        end--;
      }
      const height = end === ratios.length ? Math.min(targetHeight, width / sum) : width / sum;
      let x = 0;
      for (let index = start; index < end; index++) {
        const coverWidth = ratios[index] * height;
        placements.push({ x, y, width: coverWidth, height });
        x += coverWidth;
      }
      y += height;
      start = end;
    }
    return { placements, height: y };
  }
}
