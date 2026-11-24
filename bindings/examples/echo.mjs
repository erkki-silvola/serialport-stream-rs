// Usage: node examples/echo.mjs <path> [baud] [message]
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const { open } = require('../wrapper.js')

const [path, baud = '115200', message = 'PING\r\n'] = process.argv.slice(2)
if (!path) {
  console.error('usage: node examples/echo.mjs <path> [baud] [message]')
  process.exit(1)
}

const port = open(path, Number(baud))
process.on('SIGINT', () => port.close())

await port.write(Buffer.from(message))
try {
  for await (const chunk of port) process.stdout.write(chunk)
} catch (e) {
  if (e.message !== 'port closed') throw e
}
