// Stream reads via 'data' events, the Node.js counterpart of the `tokio_read_stream` Rust example.
// Usage: node examples/read_stream.mjs <path> <baud> [--reopen]
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const { open } = require('../wrapper.js')

const args = process.argv.slice(2)
const reopen = args.includes('--reopen')
const [path, baud] = args.filter((a) => a !== '--reopen')
if (!path || !baud) {
  console.error('usage: node examples/read_stream.mjs <path> <baud> [--reopen]')
  process.exit(1)
}

console.log(`Opening serial port ${path} at ${baud} baud`)
const port = open(path, Number(baud), {
  dtrOnOpen: true,
  autoReopen: reopen,
  reopenDelayMs: 500,
})
if (reopen) {
  port.on('disconnect', (err) => console.log(`disconnect: ${err.message}`))
  port.on('reconnecting', () => console.log('reconnecting…'))
  port.on('reopen', () => console.log('reopened'))
}
console.log('Serial port opened successfully')

process.on('SIGINT', () => {
  console.log('\nCtrl+C received, closing...')
  port.close()
})

console.log('Stream from serial port... (Press Ctrl+C to stop)')
console.log('-'.repeat(40))

port.on('data', (data) => console.log(`Received ${data.length} bytes`))
port.on('error', (err) => console.error(`poll next error: ${err.message}`))
