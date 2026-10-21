import { parentPort, workerData } from 'node:worker_threads';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { connect } from 'node:net';

const marker = join(workerData.cwd, 'startup-attempt');
parentPort.on('message', message => {
    if (message.type === 'init') {
        if (existsSync(marker)) {
            parentPort.postMessage({ type: 'ready' });
        } else {
            writeFileSync(marker, 'started');
            const port = Number(readFileSync(join(workerData.cwd, 'startup-port'), 'utf8'));
            const socket = connect({ host: '127.0.0.1', port }, () => socket.end());
        }
    } else if (message.type === 'run') {
        parentPort.postMessage({ type: 'result', cellId: message.cellId, ok: true, durationMs: 0 });
    }
});
