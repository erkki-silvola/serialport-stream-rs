'use strict'

const { EventEmitter } = require('node:events')
const native = require('./index.js')

class SerialPort extends EventEmitter {
  #port
  #closing = null

  constructor(port) {
    super()
    this.#port = port
    this.on('newListener', (event) => {
      if (event === 'data' && this.listenerCount('data') === 0 && !this.#closing) this.#startData()
    })
    this.on('removeListener', (event) => {
      if (event === 'data' && this.listenerCount('data') === 0 && !this.#closing) this.#port.stopData()
    })
  }

  #startData() {
    this.#port.startData((err, data) => {
      if (err) {
        this.close().catch(() => {})
        this.emit('error', err)
      } else {
        this.emit('data', data)
      }
    })
  }

  write(data) {
    return this.#port.write(data)
  }

  read(maxLen) {
    return this.#port.read(maxLen)
  }

  close() {
    this.#closing ??= this.#port.close().then(() => {
      this.emit('close')
    })
    return this.#closing
  }

  async *[Symbol.asyncIterator]() {
    while (true) yield await this.read()
  }
}

function open(path, baudRate, options) {
  return new SerialPort(native.open(path, baudRate, options))
}

module.exports = { open, SerialPort }
