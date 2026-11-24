'use strict'

const { EventEmitter } = require('node:events')
const native = require('./index.js')

const DEFAULT_REOPEN_DELAY_MS = 500

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function isPortClosed(err) {
  return err != null && typeof err.message === 'string' && err.message === 'port closed'
}

class SerialPort extends EventEmitter {
  #port
  #path
  #baudRate
  #nativeOptions
  #autoReopen
  #reopenDelayMs
  #userClosed = false
  #closing = null
  #reopenLoop = null

  constructor(port, config) {
    super()
    this.#port = port
    this.#path = config.path
    this.#baudRate = config.baudRate
    this.#nativeOptions = config.nativeOptions
    this.#autoReopen = config.autoReopen
    this.#reopenDelayMs = config.reopenDelayMs
    this.on('newListener', (event) => {
      if (event === 'data' && this.listenerCount('data') === 0 && !this.#closing && !this.#userClosed) {
        this.#startData()
      }
    })
    this.on('removeListener', (event) => {
      if (event === 'data' && this.listenerCount('data') === 0 && !this.#closing) {
        this.#port.stopData()
      }
    })
  }

  #startData() {
    this.#port.startData((err, data) => {
      if (err) {
        void this.#onIoError(err)
        return
      }
      this.emit('data', data)
    })
  }

  async #onIoError(err) {
    if (isPortClosed(err)) return
    this.emit('error', err)
    if (this.#autoReopen && !this.#userClosed) {
      this.emit('disconnect', err)
      await this.#reopen()
      return
    }
    await this.close()
  }

  async #reopen() {
    if (this.#userClosed || !this.#autoReopen) return
    if (this.#reopenLoop) return this.#reopenLoop

    this.#reopenLoop = (async () => {
      try {
        this.#port.stopData()
        await this.#port.close().catch(() => {})
        this.#closing = null

        while (this.#autoReopen && !this.#userClosed) {
          this.emit('reconnecting')
          await delay(this.#reopenDelayMs)
          try {
            this.#port = native.open(this.#path, this.#baudRate, this.#nativeOptions)
            this.emit('reopen')
            if (this.listenerCount('data') > 0) this.#startData()
            return
          } catch (err) {
            this.emit('error', err)
          }
        }
      } finally {
        this.#reopenLoop = null
      }
    })()

    return this.#reopenLoop
  }

  async #whenReady() {
    if (this.#reopenLoop) await this.#reopenLoop
    if (this.#userClosed) throw new Error('port closed')
  }

  async #withReopen(run) {
    await this.#whenReady()
    try {
      return await run()
    } catch (err) {
      if (this.#userClosed || isPortClosed(err)) throw err
      if (!this.#autoReopen) throw err
      await this.#onIoError(err)
      await this.#whenReady()
      return run()
    }
  }

  write(data) {
    return this.#withReopen(() => this.#port.write(data))
  }

  read(maxLen) {
    return this.#withReopen(() => this.#port.read(maxLen))
  }

  close() {
    this.#userClosed = true
    if (this.#reopenLoop) {
      const loop = this.#reopenLoop
      this.#reopenLoop = null
      return loop
        .catch(() => {})
        .then(() => this.#closePort())
    }
    return this.#closePort()
  }

  #closePort() {
    this.#closing ??= this.#port.close().then(() => {
      this.emit('close')
    })
    return this.#closing
  }

  async *[Symbol.asyncIterator]() {
    while (!this.#userClosed) yield await this.read()
  }
}

function open(path, baudRate, options = {}) {
  const {
    autoReopen = false,
    reopenDelayMs = DEFAULT_REOPEN_DELAY_MS,
    ...nativeOptions
  } = options
  const port = native.open(path, baudRate, nativeOptions)
  return new SerialPort(port, {
    path,
    baudRate,
    nativeOptions,
    autoReopen: Boolean(autoReopen),
    reopenDelayMs: Math.max(0, Number(reopenDelayMs) || DEFAULT_REOPEN_DELAY_MS),
  })
}

module.exports = { open, SerialPort }