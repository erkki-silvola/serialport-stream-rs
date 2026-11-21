import { EventEmitter } from 'node:events'
import type { OpenOptions } from './index'

export type { OpenOptions } from './index'

/** Opens a serial port, e.g. `open('/dev/ttyUSB0', 115200)` or `open('COM3', 9600, { parity: 'even' })`. */
export declare function open(path: string, baudRate: number, options?: OpenOptions | null): SerialPort

/**
 * Open serial port. Receive with `read()` / `for await`, or with `'data'` listeners, not both:
 * `read()` rejects while any `'data'` listener is attached.
 */
export declare class SerialPort extends EventEmitter implements AsyncIterable<Buffer> {
  private constructor()
  /** Writes all bytes and waits until they are transmitted. */
  write(data: Buffer): Promise<void>
  /** Resolves with the next received bytes, at most `maxLen`. */
  read(maxLen?: number | null): Promise<Buffer>
  /** Stops pending operations, closes the port, and emits `'close'` once. */
  close(): Promise<void>
  [Symbol.asyncIterator](): AsyncGenerator<Buffer, never, unknown>

  on(event: 'data', listener: (data: Buffer) => void): this
  on(event: 'error', listener: (err: Error) => void): this
  on(event: 'close', listener: () => void): this
  on(event: string | symbol, listener: (...args: any[]) => void): this
  once(event: 'data', listener: (data: Buffer) => void): this
  once(event: 'error', listener: (err: Error) => void): this
  once(event: 'close', listener: () => void): this
  once(event: string | symbol, listener: (...args: any[]) => void): this
  off(event: 'data', listener: (data: Buffer) => void): this
  off(event: 'error', listener: (err: Error) => void): this
  off(event: 'close', listener: () => void): this
  off(event: string | symbol, listener: (...args: any[]) => void): this
}
