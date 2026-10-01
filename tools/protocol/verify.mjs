import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const verifier = path.join(path.dirname(fileURLToPath(import.meta.url)), 'verify_java_263.py');
const result = spawnSync(process.env.PYTHON ?? 'python3', [verifier, ...process.argv.slice(2)], {
  stdio: 'inherit',
});

if (result.error) {
  console.error(result.error);
  process.exit(1);
}

process.exit(result.status ?? 1);
