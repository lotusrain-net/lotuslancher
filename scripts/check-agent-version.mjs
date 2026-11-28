import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { hasDevelopmentBuildMarker } from './agent-binary.mjs';

const sidecarPath = fileURLToPath(new URL('../src-tauri/binaries/agent-x86_64-pc-windows-msvc.exe', import.meta.url));

const sidecarBytes = await readFile(sidecarPath);
if (hasDevelopmentBuildMarker(sidecarBytes)) {
  process.stderr.write('Embedded agent contains development-only build markers.\n');
  process.exitCode = 1;
}

if (process.platform === 'win32') {
  const packageUrl = new URL('../package.json', import.meta.url);
  const packageInfo = JSON.parse(await readFile(packageUrl, 'utf8'));
  const result = spawnSync(sidecarPath, ['--version'], { encoding: 'utf8', windowsHide: true });
  const agentVersion = result.stdout.trim();

  if (result.error || result.status !== 0 || agentVersion !== packageInfo.version) {
    process.stderr.write(`Embedded agent version ${agentVersion || 'unavailable'} does not match desktop version ${packageInfo.version}.\n`);
    process.exitCode = 1;
  }
}
