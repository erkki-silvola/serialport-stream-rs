# serialport-stream for Node.js

Node.js native addon (napi-rs) for the [`serialport-stream`](../README.md) Rust crate. It works on macOS, Linux, and Windows.

## Build

```bash
npm install
npm run build        # release build, writes serialport-stream-rs.<platform>.node
```

## Usage

```js
const { open } = require('serialport-stream-rs')

const port = open('/dev/ttyUSB0', 115200, { parity: 'none', dataBits: 8 })
await port.write(Buffer.from('PING\r\n'))

const reply = await port.read()          // up to 256 bytes
for await (const chunk of port) {        // or iterate
  console.log(chunk)
}
await port.close()
```

`SerialPort` is an `EventEmitter`. As an alternative to `read()` and `for await`, listen for events:

```js
port.on('data', (data) => console.log(data))
port.on('error', (err) => console.error(err))   // fatal read error; the port is closed
port.on('close', () => console.log('closed'))
```

The first `'data'` listener starts the receive loop, and removing the last one stops it. Unread bytes stay buffered. While any `'data'` listener is attached, `read()` rejects. Node's usual rule applies: an `'error'` event with no listener throws.

## API

| Item | Description |
| --- | --- |
| `open(path, baudRate, options?)` | Opens the port. Options: `dataBits` (5-8), `parity` (`'none'`, `'odd'`, `'even'`), `stopBits` (1, 2), `flowControl` (`'none'`, `'software'`, `'hardware'`), `dtrOnOpen`, `clear` (`'input'`, `'output'`, `'all'`). |
| `write(data)` | Writes all bytes and resolves after they are transmitted. |
| `read(maxLen?)` | Resolves with the next received bytes. |
| `close()` | Rejects pending operations with `port closed`, releases the port, and emits `'close'`. |
| `'data'` event | Each received chunk, as a `Buffer`. |
| `'error'` event | Fatal read error. The port is closed afterwards. |
| `'close'` event | Emitted once after the port closes. |

## Examples

| Example | Description |
| --- | --- |
| `async_rw.mjs` | Write / read loop, same as the Rust `tokio_async_rw` example |
| `read_stream.mjs` | Prints received chunk sizes from `'data'` events, same as the Rust `tokio_read_stream` example |
| `echo.mjs` | Writes a message and prints everything received |

```bash
node examples/async_rw.mjs /dev/ttyUSB0 115200
node examples/read_stream.mjs /dev/ttyUSB0 115200
node examples/echo.mjs /dev/ttyUSB0 115200 "PING\r\n"
```
