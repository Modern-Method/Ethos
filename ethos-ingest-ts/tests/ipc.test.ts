import * as net from 'net';
import * as path from 'path';
import * as fs from 'fs';
import msgpack5 from 'msgpack5';

const msgpack = msgpack5();

describe('Ethos Ingest IPC Framing', () => {
    const SOCKET_PATH = path.join('/tmp', `ethos-test-${Date.now()}.sock`);

    beforeAll(() => {
        if (fs.existsSync(SOCKET_PATH)) {
            fs.unlinkSync(SOCKET_PATH);
        }
    });

    afterAll(() => {
        if (fs.existsSync(SOCKET_PATH)) {
            fs.unlinkSync(SOCKET_PATH);
        }
    });

    it('should send a MessagePack payload with a 4-byte Little Endian length prefix', (done) => {
        const testPayload = { action: 'ingest', payload: { content: 'Hello Ethos', source: 'user' } };
        
        const server = net.createServer((socket) => {
            socket.on('data', (data: Buffer) => {
                // Verify length prefix (4 bytes, Little Endian)
                expect(data.length).toBeGreaterThan(4);
                const length = data.readUInt32LE(0);
                const payload = data.subarray(4);
                
                expect(length).toBe(payload.length);
                
                // Verify MessagePack payload
                const decoded = msgpack.decode(payload);
                expect(decoded).toEqual(testPayload);
                
                server.close();
                done();
            });
        });

        server.listen(SOCKET_PATH, () => {
            const client = net.createConnection(SOCKET_PATH, () => {
                const encoded = msgpack.encode(testPayload);
                const header = Buffer.alloc(4);
                header.writeUInt32LE(encoded.length, 0);
                
                // encoded is a BufferListStream, we need to convert to Buffer
                client.write(Buffer.concat([header, Buffer.from(encoded.slice())]));
                client.end();
            });
        });
    });
});
