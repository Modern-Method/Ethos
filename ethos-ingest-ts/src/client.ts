import * as net from 'net';
import msgpack5 from 'msgpack5';
import { EventEmitter } from 'events';

const msgpack = msgpack5();

export type EthosSource = 'user' | 'assistant' | 'system';

// Rust IPC protocol: {action: "ingest", payload: {...}}
// Matches ethos_core::ipc::EthosRequest enum tagged with "action" field
export interface IngestRequest {
    action: 'ingest';
    payload: {
        content: string;
        source: EthosSource;
        metadata?: Record<string, any>;
    };
}

export interface SearchRequest {
    action: 'search';
    query: string;
    limit?: number;
}

export type EthosRequest = IngestRequest | SearchRequest;

export interface SearchResponse {
    status: string;
    data?: {
        results: Array<{
            id: string;
            content: string;
            source: string;
            score: number;
            metadata: Record<string, any>;
            created_at: string;
        }>;
        query: string;
        count: number;
    };
    error?: string;
}

export interface EthosClientOptions {
    socketPath: string;
    retryInterval?: number;
    maxRetryInterval?: number;
}

/**
 * EthosClient for communicating with the Ethos IPC server
 * 
 * Supports both fire-and-forget (send) and request-response (request) patterns.
 */
export class EthosClient extends EventEmitter {
    private socket: net.Socket | null = null;
    private connecting = false;
    private options: Required<EthosClientOptions>;
    private requestId = 0;
    private pendingRequests: Map<number, { resolve: Function; reject: Function; timeout: NodeJS.Timeout }> = new Map();
    private buffer: Buffer = Buffer.alloc(0);

    constructor(options: EthosClientOptions) {
        super();
        this.options = {
            retryInterval: 1000,
            maxRetryInterval: 60000,
            ...options
        };
    }

    public async connect(): Promise<void> {
        if (this.socket || this.connecting) return;
        this.connecting = true;

        return new Promise((resolve) => {
            const attempt = (interval: number) => {
                const socket = net.createConnection(this.options.socketPath);

                socket.on('connect', () => {
                    this.socket = socket;
                    this.connecting = false;
                    console.log(`Connected to Ethos at ${this.options.socketPath}`);
                    resolve();
                });

                socket.on('error', (err) => {
                    this.socket = null;
                    const nextInterval = Math.min(interval * 2, this.options.maxRetryInterval);
                    setTimeout(() => attempt(nextInterval), interval);
                });

                socket.on('close', () => {
                    this.socket = null;
                    this.emit('close');
                    if (!this.connecting) {
                        this.connect(); // Try to reconnect
                    }
                });

                // Handle incoming responses for request-response pattern
                socket.on('data', (data: Buffer) => {
                    this.handleData(data);
                });
            };

            attempt(this.options.retryInterval);
        });
    }

    /**
     * Fire-and-forget send (used for ingest operations)
     */
    public send(request: EthosRequest): boolean {
        if (!this.socket) {
            return false;
        }

        try {
            const encoded = msgpack.encode(request);
            const header = Buffer.allocUnsafe(4);
            header.writeUInt32LE(encoded.length, 0);

            // Using Buffer.from(encoded.slice()) to get a flat Buffer from BufferListStream
            this.socket.write(header);
            this.socket.write(Buffer.from(encoded.slice()));
            return true;
        } catch (err) {
            console.error('Failed to send Ethos request:', err);
            return false;
        }
    }

    /**
     * Request-response pattern (used for search operations)
     * 
     * Sends a request and waits for a response with timeout.
     */
    public async request<T = any>(request: EthosRequest, timeoutMs: number = 5000): Promise<T> {
        return new Promise((resolve, reject) => {
            if (!this.socket) {
                reject(new Error('Not connected to Ethos'));
                return;
            }

            const id = ++this.requestId;
            
            // Set up timeout
            const timeout = setTimeout(() => {
                this.pendingRequests.delete(id);
                reject(new Error(`Request ${id} timed out after ${timeoutMs}ms`));
            }, timeoutMs);

            // Store pending request
            this.pendingRequests.set(id, { resolve, reject, timeout });

            try {
                const encoded = msgpack.encode(request);
                const header = Buffer.allocUnsafe(4);
                header.writeUInt32LE(encoded.length, 0);

                this.socket.write(header);
                this.socket.write(Buffer.from(encoded.slice()));
            } catch (err) {
                this.pendingRequests.delete(id);
                clearTimeout(timeout);
                reject(err);
            }
        });
    }

    /**
     * Handle incoming data frames (LE length prefix + MessagePack)
     */
    private handleData(data: Buffer): void {
        // Append to buffer
        this.buffer = Buffer.concat([this.buffer, data]);

        // Try to extract complete frames
        while (this.buffer.length >= 4) {
            // Read length prefix (Little Endian u32)
            const frameLength = this.buffer.readUInt32LE(0);

            // Check if we have the complete frame
            if (this.buffer.length < 4 + frameLength) {
                break; // Wait for more data
            }

            // Extract frame payload
            const payload = this.buffer.slice(4, 4 + frameLength);
            this.buffer = this.buffer.slice(4 + frameLength);

            // Decode MessagePack
            try {
                const response = msgpack.decode(payload);
                this.emit('response', response);

                // Resolve pending requests (FIFO - first pending request gets this response)
                const firstPending = this.pendingRequests.entries().next();
                if (!firstPending.done) {
                    const [id, { resolve, timeout }] = firstPending.value;
                    clearTimeout(timeout);
                    this.pendingRequests.delete(id);
                    resolve(response);
                }
            } catch (err) {
                console.error('Failed to decode response:', err);
            }
        }
    }

    public destroy(): void {
        if (this.socket) {
            this.socket.destroy();
            this.socket = null;
        }
        // Reject all pending requests
        for (const [id, { reject, timeout }] of this.pendingRequests) {
            clearTimeout(timeout);
            reject(new Error('Client destroyed'));
        }
        this.pendingRequests.clear();
    }
}
