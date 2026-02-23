import * as net from 'net';
import * as path from 'path';
import * as fs from 'fs';
import msgpack5 from 'msgpack5';
import { EthosClient } from '../src/client';

const msgpack = msgpack5();

describe('EthosClient Request-Response', () => {
    const SOCKET_PATH = path.join('/tmp', `ethos-client-req-test-${Date.now()}.sock`);

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

    it('should send request and receive response', async () => {
        const mockResponse = {
            status: 'ok',
            data: {
                results: [
                    {
                        id: 'test-uuid',
                        content: 'test memory',
                        source: 'user',
                        score: 0.95,
                        metadata: {},
                        created_at: new Date().toISOString()
                    }
                ],
                query: 'test query',
                count: 1
            }
        };

        const server = net.createServer((socket) => {
            // Read request frame
            let buffer = Buffer.alloc(0);
            socket.on('data', (data: Buffer) => {
                buffer = Buffer.concat([buffer, data]);
                
                if (buffer.length >= 4) {
                    const frameLength = buffer.readUInt32LE(0);
                    if (buffer.length >= 4 + frameLength) {
                        // Send response
                        const encoded = msgpack.encode(mockResponse);
                        const header = Buffer.allocUnsafe(4);
                        header.writeUInt32LE(encoded.length, 0);
                        socket.write(header);
                        socket.write(Buffer.from(encoded.slice()));
                    }
                }
            });
        });

        await new Promise<void>((resolve) => server.listen(SOCKET_PATH, resolve));

        const client = new EthosClient({ socketPath: SOCKET_PATH });
        await client.connect();

        const response = await client.request({ action: 'search', payload: { query: 'test' } });
        
        expect(response.status).toBe('ok');
        expect(response.data.results).toHaveLength(1);
        expect(response.data.results[0].content).toBe('test memory');
        expect(response.data.results[0].score).toBe(0.95);

        client.destroy();
        server.close();
    });

    it('should timeout if no response received', async () => {
        const server = net.createServer((socket) => {
            // Don't send any response
        });

        await new Promise<void>((resolve) => server.listen(SOCKET_PATH, resolve));

        const client = new EthosClient({ socketPath: SOCKET_PATH });
        await client.connect();

        await expect(
            client.request({ action: 'search', payload: { query: 'test' } }, 500)
        ).rejects.toThrow('timed out');

        client.destroy();
        server.close();
    });

    it('should reject if not connected', async () => {
        const client = new EthosClient({ socketPath: SOCKET_PATH });
        
        await expect(
            client.request({ action: 'search', payload: { query: 'test' } })
        ).rejects.toThrow('Not connected to Ethos');

        client.destroy();
    });

    it('should handle multiple sequential requests', async () => {
        let requestCount = 0;

        const server = net.createServer((socket) => {
            let buffer = Buffer.alloc(0);
            socket.on('data', (data: Buffer) => {
                buffer = Buffer.concat([buffer, data]);
                
                while (buffer.length >= 4) {
                    const frameLength = buffer.readUInt32LE(0);
                    if (buffer.length < 4 + frameLength) break;
                    
                    requestCount++;
                    const response = {
                        status: 'ok',
                        data: { results: [], query: 'test', count: 0, requestNumber: requestCount }
                    };
                    
                    const encoded = msgpack.encode(response);
                    const header = Buffer.allocUnsafe(4);
                    header.writeUInt32LE(encoded.length, 0);
                    socket.write(header);
                    socket.write(Buffer.from(encoded.slice()));
                    
                    buffer = buffer.slice(4 + frameLength);
                }
            });
        });

        await new Promise<void>((resolve) => server.listen(SOCKET_PATH, resolve));

        const client = new EthosClient({ socketPath: SOCKET_PATH });
        await client.connect();

        const res1 = await client.request({ action: 'search', payload: { query: 'test1' } });
        expect(res1.data.requestNumber).toBe(1);

        const res2 = await client.request({ action: 'search', payload: { query: 'test2' } });
        expect(res2.data.requestNumber).toBe(2);

        const res3 = await client.request({ action: 'search', payload: { query: 'test3' } });
        expect(res3.data.requestNumber).toBe(3);

        client.destroy();
        server.close();
    });

    it('should reject pending requests on destroy', async () => {
        const server = net.createServer((socket) => {
            // Don't respond
        });

        await new Promise<void>((resolve) => server.listen(SOCKET_PATH, resolve));

        const client = new EthosClient({ socketPath: SOCKET_PATH });
        await client.connect();

        const requestPromise = client.request({ action: 'search', payload: { query: 'test' } }, 10000);
        
        // Destroy immediately
        client.destroy();

        await expect(requestPromise).rejects.toThrow('Client destroyed');

        server.close();
    });
});
