import { EthosClient } from './client';

// OpenClaw hook entry point
export async function setup(context: any) {
    const socketPath = process.env.ETHOS_SOCKET || '/tmp/ethos.sock';
    const client = new EthosClient({ socketPath });

    // Start connection in background
    client.connect().catch(err => {
        console.error('Ethos connection failed:', err);
    });

    context.on('message:received', (msg: any) => {
        client.send({
            action: 'ingest',
            payload: {
                content: msg.content,
                source: 'user',
                metadata: {
                    channel: msg.channel,
                    author: msg.author,
                    ts: new Date().toISOString()
                }
            }
        });
    });

    context.on('message:sent', (msg: any) => {
        client.send({
            action: 'ingest',
            payload: {
                content: msg.content,
                source: 'assistant',
                metadata: {
                    channel: msg.channel,
                    ts: new Date().toISOString()
                }
            }
        });
    });

    return () => {
        client.destroy();
    };
}
