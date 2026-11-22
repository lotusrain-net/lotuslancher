// LotusProxy mark: five petals fanned out from one focus point, built from
// the golden ratio φ so the proportions are derived, not eyeballed.
//
// * The mark's bounding box is a golden rectangle (width : height = φ).
// * Every petal's side corners sit on one ring of radius R/φ around the focus.
// * The lower petals' base corner sits at R/φ^1.5; the mark fills 1 − 1/φ⁴ of
//   the canvas width (the old ShardX icon's footprint).
// * Corner radius = gap / φ, so the rounding reads as part of the gap rhythm.
//
// `buildSvg({ size, minGapPx })` returns an SVG string. `minGapPx` widens the
// gaps at small sizes so the petals stay apart once rasterized at 16–32 px.

export const PHI = (1 + Math.sqrt(5)) / 2;
export const COLOR = "#535EFD";

const rad = (d) => (d * Math.PI) / 180;
const sub = (a, b) => [a[0] - b[0], a[1] - b[1]];
const add = (a, b) => [a[0] + b[0], a[1] + b[1]];
const mul = (a, k) => [a[0] * k, a[1] * k];
const len = (a) => Math.hypot(a[0], a[1]);
const unit = (a) => mul(a, 1 / len(a));
const cross = (a, b) => a[0] * b[1] - a[1] * b[0];

/** Point at `deg` from straight up (positive = left), `r` from the focus. */
const polar = (deg, r) => [-Math.sin(rad(deg)) * r, -Math.cos(rad(deg)) * r];
const mirror = (p) => [-p[0], p[1]];

/** Petal outlines around the focus (0,0), R = 1. Edge flags: true = divider (inset by gap/2). */
function skeleton(R2) {
  const R0 = 1;
  const ring = R0 / PHI;
  const rb = R0 / Math.pow(PHI, 1.5);
  const A = { c: 24.5, top: 39, l: 59.7, tip: 78.6, base: 99.8 };
  const O = [0, 0];
  const center = {
    pts: [polar(0, R0), polar(A.c, ring), O, polar(-A.c, ring)],
    div: [false, true, true, false],
  };
  const upper = {
    pts: [polar(A.c, ring), polar(A.top, R0 * 0.941), polar(A.l, ring), O],
    div: [false, false, true, true],
  };
  const lower = {
    pts: [polar(A.l, ring), polar(A.tip, R2), polar(A.base, rb), O],
    div: [false, false, false, true],
  };
  const flip = (p) => ({ pts: p.pts.map(mirror).reverse(), div: [...p.div.slice(0, -1).reverse(), p.div[p.div.length - 1]] });
  return [center, upper, flip(upper), lower, flip(lower)];
}

/** Move divider edges inward by `inset`; vertices become offset-line intersections. */
function insetPolygon({ pts, div }, inset) {
  const n = pts.length;
  let area = 0;
  for (let i = 0; i < n; i++) area += cross(pts[i], pts[(i + 1) % n]);
  const sign = area > 0 ? 1 : -1;
  const lines = pts.map((p, i) => {
    const q = pts[(i + 1) % n];
    const d = unit(sub(q, p));
    const inward = mul([-d[1], d[0]], sign);
    const o = div[i] ? mul(inward, inset) : [0, 0];
    return { p: add(p, o), d };
  });
  return lines.map((l, i) => {
    const m = lines[(i + n - 1) % n];
    const t = cross(sub(l.p, m.p), l.d) / cross(m.d, l.d);
    return add(m.p, mul(m.d, t));
  });
}

/** Path with every corner filleted by a circular arc of radius `r` (clamped on short edges). */
function roundedPath(pts, r, fmt) {
  const n = pts.length;
  let d = "";
  pts.forEach((v, i) => {
    const p = pts[(i + n - 1) % n];
    const q = pts[(i + 1) % n];
    const u1 = unit(sub(p, v));
    const u2 = unit(sub(q, v));
    const theta = Math.acos(Math.max(-1, Math.min(1, u1[0] * u2[0] + u1[1] * u2[1])));
    const maxD = 0.42 * Math.min(len(sub(p, v)), len(sub(q, v)));
    let t = r / Math.tan(theta / 2);
    let rr = r;
    if (t > maxD) {
      t = maxD;
      rr = t * Math.tan(theta / 2);
    }
    const s = add(v, mul(u1, t));
    const e = add(v, mul(u2, t));
    const sweep = cross(sub(v, p), sub(q, v)) > 0 ? 1 : 0;
    d += `${i ? "L" : "M"}${fmt(s)}A${rr.toFixed(3)} ${rr.toFixed(3)} 0 0 ${sweep} ${fmt(e)}`;
  });
  return d + "Z";
}

function geometry(R2, gap, radius) {
  return skeleton(R2).map((p) => insetPolygon(p, gap / 2)).map((pts) => ({ pts, radius }));
}

/** Bounding box of the polygons (corners rounded off barely move it; good enough to solve R2). */
function bbox(polys) {
  const xs = polys.flatMap((p) => p.pts.map((v) => v[0]));
  const ys = polys.flatMap((p) => p.pts.map((v) => v[1]));
  return { x0: Math.min(...xs), x1: Math.max(...xs), y0: Math.min(...ys), y1: Math.max(...ys) };
}

/** The base gap in focus units (R = 1). */
export const BASE_GAP = 0.0607;

export function buildSvg({ size = 512, minGapPx = 0, color = COLOR, fill = 1 - 1 / Math.pow(PHI, 4) } = {}) {
  const markW = size * fill;
  // Solve the lower petals' reach so the box is a golden rectangle.
  const solve = (gap) => {
    let lo = 0.6, hi = 1.2;
    for (let k = 0; k < 60; k++) {
      const mid = (lo + hi) / 2;
      const b = bbox(geometry(mid, gap, 0));
      (b.x1 - b.x0) / (b.y1 - b.y0) > PHI ? (hi = mid) : (lo = mid);
    }
    return (lo + hi) / 2;
  };
  let gap = BASE_GAP;
  let R2 = solve(gap);
  let b = bbox(geometry(R2, gap, 0));
  let scale = markW / (b.x1 - b.x0);
  if (gap * scale < minGapPx) {
    gap = minGapPx / scale;
    R2 = solve(gap);
    b = bbox(geometry(R2, gap, 0));
    scale = markW / (b.x1 - b.x0);
  }
  const radius = gap / PHI;
  const ox = size / 2 - ((b.x0 + b.x1) / 2) * scale;
  const oy = size / 2 - ((b.y0 + b.y1) / 2) * scale;
  const fmt = (v) => `${v[0].toFixed(3)} ${v[1].toFixed(3)}`;
  const scaled = geometry(R2, gap, radius).map((p) =>
    roundedPath(p.pts.map((v) => [v[0] * scale + ox, v[1] * scale + oy]), radius * scale, fmt),
  );
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}"><path fill="${color}" d="${scaled.join("")}"/></svg>\n`;
}
