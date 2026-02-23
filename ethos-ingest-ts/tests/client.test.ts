import * as net from 'net';
import * as path from 'path';
import * as fs from 'fs';
import { EthosClient } from '../src/client';

describe('EthosClient', () => {
    const SOCKET_PATH = path.join('/tmp', `ethos-client-test-${Date.now()}.sock`);

    beforeEach(() => {
        if (fs.existsSync(SOCKET_PATH)) {
            fs.unlinkSync(SOCKET_PATH);
        }
    });

    afterEach(() => {
        if (fs.existsSync(SOCKET_PATH)) {
            fs.unlinkSync(SOCKET_PATH);
        }
    });

    it('should connect and send messages', (done) => {
        const server = net.createServer((socket) => {
            socket.on('data', (data) => {
                expect(data.length).toBeGreaterThan(4);
                done();
            });
        });

        server.listen(SOCKET_PATH, async () => {
            const client = new EthosClient({ socketPath: SOCKET_PATH });
            await client.connect();
            const success = client.send({ action: 'ingest', payload: { content: 'test', source: 'user' } });
            expect(success).toBe(true);
            client.destroy();
            server.close();
        });
    });

    it('should retry connection if server is not yet up', (done) => {
        const client = new EthosClient({ socketPath: SOCKET_PATH, retryInterval: 100 });
        client.connect();

        setTimeout(() => {
            const server = net.createServer((socket) => {
                socket.on('data', () => {
                    client.destroy();
                    server.close();
                    done();
                });
            });

            server.listen(SOCKET_PATH, () => {
                // Should eventually connect and be able to send
                const check = setInterval(() => {
                    if (client.send({ action: 'ingest', payload: { content: 'retry-test', source: 'assistant' } })) {
                        clearInterval(check);
                    }
                }, 50);
            });
        }, 300);
    });
});
