import * as net from 'net';
import * as path from 'path';
import * as fs from 'fs';
import { setup } from '../src/index';
import EventEmitter from 'events';
import msgpack5 from 'msgpack5';

const msgpack = msgpack5();

describe('Ethos Ingest Setup', () => {
    const SOCKET_PATH = path.join('/tmp', `ethos-setup-test-${Date.now()}.sock`);

    beforeEach(() => {
        process.env.ETHOS_SOCKET = SOCKET_PATH;
        if (fs.existsSync(SOCKET_PATH)) {
            try { fs.unlinkSync(SOCKET_PATH); } catch (e) {}
        }
    });

    afterEach(() => {
        if (fs.existsSync(SOCKET_PATH)) {
            try { fs.unlinkSync(SOCKET_PATH); } catch (e) {}
        }
    });

    it('should handle message:received and message:sent', (done) => {
        const receivedMessages: any[] = [];
        const server = net.createServer((socket) => {
            socket.on('data', (data: Buffer) => {
                let offset = 0;
                while (offset < data.length) {
                    if (data.length < offset + 4) break;
                    const length = data.readUInt32LE(offset);
                    if (data.length < offset + 4 + length) break;
                    const payload = data.subarray(offset + 4, offset + 4 + length);
                    receivedMessages.push(msgpack.decode(payload));
                    offset += 4 + length;
                }
                
                if (receivedMessages.length === 2) {
                    // New format: {action:'ingest', payload:{content, source, metadata}}
                    expect(receivedMessages[0].action).toBe('ingest');
                    expect(receivedMessages[0].payload.content).toBe('hello');
                    expect(receivedMessages[0].payload.source).toBe('user');
                    expect(receivedMessages[1].action).toBe('ingest');
                    expect(receivedMessages[1].payload.content).toBe('hi there');
                    expect(receivedMessages[1].payload.source).toBe('assistant');
                    server.close();
                    done();
                }
            });
        });

        server.listen(SOCKET_PATH, async () => {
            const context = new EventEmitter();
            const cleanup = await setup(context);

            // Wait a bit for connection to be established
            setTimeout(() => {
                context.emit('message:received', { content: 'hello', channel: 'test', author: 'user1' });
                context.emit('message:sent', { content: 'hi there', channel: 'test' });
            }, 100);

            expect(typeof cleanup).toBe('function');
        });
    }, 10000);
});
