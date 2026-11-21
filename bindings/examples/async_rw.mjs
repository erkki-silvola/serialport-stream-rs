// Async read/write loop, the Node.js counterpart of the `tokio_async_rw` Rust example.
// Usage: node examples/async_rw.mjs <path> <baud>
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const { open } = require('../wrapper.js')

const WRITE_PAYLOAD = Buffer.from([0x0a, 0xc0])

const [path, baud] = process.argv.slice(2)
if (!path || !baud) {
  console.error('usage: node examples/async_rw.mjs <path> <baud>')
  process.exit(1)
}

console.log(`Opening ${path} @ ${baud} baud`)
const port = open(path, Number(baud), { dtrOnOpen: true })

let stopping = false
process.on('SIGINT', () => {
  console.log('\nCtrl+C — exiting')
  stopping = true
  port.close()
})

console.log('Read / write with Promise read() + write()')
console.log('-'.repeat(80))

const hex = (buf) => `[${[...buf].map((b) => b.toString(16).toUpperCase().padStart(2, '0')).join(', ')}]`

while (!stopping) {
  try {
    await port.write(WRITE_PAYLOAD)
    console.log(`payload ${hex(WRITE_PAYLOAD)} written and flushed`)
    const data = await port.read(256)
    console.log(`received ${data.length} bytes: ${hex(data)}`)
  } catch (e) {
    if (!stopping) console.error(`read/write error: ${e.message}`)
    break
  }
}
await port.close()
