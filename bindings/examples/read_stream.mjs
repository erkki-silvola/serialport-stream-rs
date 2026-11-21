// Stream reads via 'data' events, the Node.js counterpart of the `tokio_read_stream` Rust example.
// Usage: node examples/read_stream.mjs <path> <baud>
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const { open } = require('../wrapper.js')

const [path, baud] = process.argv.slice(2)
if (!path || !baud) {
  console.error('usage: node examples/read_stream.mjs <path> <baud>')
  process.exit(1)
}

console.log(`Opening serial port ${path} at ${baud} baud`)
const port = open(path, Number(baud), { dtrOnOpen: true })
console.log('Serial port opened successfully')

process.on('SIGINT', () => {
  console.log('\nCtrl+C received, closing...')
  port.close()
})

console.log('Stream from serial port... (Press Ctrl+C to stop)')
console.log('-'.repeat(40))

port.on('data', (data) => console.log(`Received ${data.length} bytes`))
port.on('error', (err) => console.error(`poll next error: ${err.message}`))
