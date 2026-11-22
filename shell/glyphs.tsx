// Title bar glyphs: one stroke (1 px), one box (12 px), drawn on the pixel
// grid (half-pixel coordinates for 1 px lines) so all four read as one set.
// The caption glyphs are Windows 11's shapes; the gear follows the same line.

const S = { fill: "none", stroke: "currentColor", strokeWidth: 1, vectorEffect: "non-scaling-stroke" } as const;

function Box({ children }: { children: React.ReactNode }) {
  return (
    <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true" shapeRendering="geometricPrecision">
      {children}
    </svg>
  );
}

export const MinimizeGlyph = () => (
  <Box>
    <path {...S} d="M1 6.5h10" />
  </Box>
);

export const MaximizeGlyph = () => (
  <Box>
    <rect {...S} x="1.5" y="1.5" width="9" height="9" rx="1.5" />
  </Box>
);

/** Two overlapping squares: the window is maximized, a click restores it. */
export const RestoreGlyph = () => (
  <Box>
    <rect {...S} x="1.5" y="3.5" width="7" height="7" rx="1.25" />
    <path {...S} d="M3.5 3.5V2.75C3.5 2.06 4.06 1.5 4.75 1.5H9.25C9.94 1.5 10.5 2.06 10.5 2.75V7.25C10.5 7.94 9.94 8.5 9.25 8.5H8.5" />
  </Box>
);

export const CloseGlyph = () => (
  <Box>
    <path {...S} strokeLinecap="round" d="M1.5 1.5l9 9M10.5 1.5l-9 9" />
  </Box>
);

/** Six teeth around a ring: an outline in the caption glyphs' weight. */
const GEAR = (() => {
  const c = 6;
  const teeth = 6;
  const rOut = 5.25;
  const rIn = 4;
  const half = Math.PI / teeth;
  const pts: string[] = [];
  for (let i = 0; i < teeth; i++) {
    const a = (i * 2 * Math.PI) / teeth - Math.PI / 2;
    // A tooth: its top is 40 % of the step, its flanks taper into the ring.
    for (const [r, da] of [[rIn, -half * 0.62], [rOut, -half * 0.34], [rOut, half * 0.34], [rIn, half * 0.62]] as const) {
      pts.push(`${(c + r * Math.cos(a + da)).toFixed(3)} ${(c + r * Math.sin(a + da)).toFixed(3)}`);
    }
  }
  return `M${pts.join("L")}Z`;
})();

export const GearGlyph = () => (
  <Box>
    <path {...S} strokeLinejoin="round" d={GEAR} />
    <circle {...S} cx="6" cy="6" r="1.75" />
  </Box>
);
