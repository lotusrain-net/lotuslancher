const DEVELOPMENT_MARKERS = ['-tags=development', 'development-only', 'verify_browser'];

export function hasDevelopmentBuildMarker(bytes) {
  const binary = Buffer.from(bytes);
  return DEVELOPMENT_MARKERS.some((marker) => binary.includes(Buffer.from(marker)));
}
