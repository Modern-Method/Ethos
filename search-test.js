const net = require('net');
const { encode, decode } = require('@msgpack/msgpack');

const socket = net.createConnection('/tmp/ethos.sock');
const frame = (obj) => {
  const payload = encode(obj);
  const header = Buffer.allocUnsafe(4);
  header.writeUInt32LE(payload.length, 0);
  return Buffer.concat([header, Buffer.from(payload)]);
};

socket.once('connect', () => {
  console.log('Sending search...');
  socket.write(frame({
    action: 'search',
    query: 'hello world',
    limit: 2
  }));

  let buf = Buffer.alloc(0);
  socket.on('data', (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    if (buf.length < 4) return;
    const len = buf.readUInt32LE(0);
    if (buf.length < 4 + len) return;
    
    const resp = decode(buf.slice(4, 4 + len));
    console.log('Search Results:', JSON.stringify(resp, null, 2));
    socket.destroy();
  });
});
