/** Node-subpath file helpers (no DOM): WAV parse + PCM16/mono extraction
 * for the CLI example and batch uploads. Node-builds only (uses Buffer). */

export interface WavInfo {
  channels: number;
  sampleRateHz: number;
  bitsPerSample: number;
  isFloat: boolean;
  /** Byte length of the data chunk. */
  dataBytes: number;
  /** Offset of the data chunk within the file. */
  dataOffset: number;
}

/** Parses RIFF chunks and the fmt/data pair. Not a strict validator. */
export function readWavHeader(bytes: Uint8Array): WavInfo {
  if (bytes.length < 12) throw new Error("not a WAV file (too short)");
  if (bytes[0] !== 0x52 || bytes[1] !== 0x49 || bytes[2] !== 0x46 || bytes[3] !== 0x46) {
    throw new Error("not a RIFF container");
  }
  if (!(bytes[8] === 0x57 && bytes[9] === 0x41 && bytes[10] === 0x56 && bytes[11] === 0x45)) {
    throw new Error("not a WAVE RIFF");
  }
  let position = 12;
  let info: WavInfo | undefined;
  while (position + 8 <= bytes.length) {
    const chunkId = String.fromCharCode(bytes[position], bytes[position + 1], bytes[position + 2], bytes[position + 3]);
    const chunkSize = readU32LE(bytes, position + 4);
    position += 8;
    if (chunkId === "fmt " && position + 16 <= bytes.length) {
      const audioFormat = readU16LE(bytes, position);
      const channels = readU16LE(bytes, position + 2);
      const sampleRateHz = readU32LE(bytes, position + 4);
      const bitsPerSample = readU16LE(bytes, position + 14);
      info = {
        channels: channels || 1,
        sampleRateHz,
        bitsPerSample,
        isFloat: audioFormat === 3,
        dataBytes: 0,
        dataOffset: 0,
      };
    } else if (chunkId === "data" && info) {
      info.dataBytes = chunkSize === 0 ? bytes.length - position : Math.min(chunkSize, bytes.length - position);
      info.dataOffset = position;
      break;
    }
    position += chunkSize + (chunkSize & 1); // RIFF chunks are word-aligned
  }
  if (!info) throw new Error("missing fmt/data chunks");
  if (info.bitsPerSample % 8 !== 0 || info.bitsPerSample < 8 || info.bitsPerSample > 32) {
    throw new Error(`unsupported bits/sample ${info.bitsPerSample}`);
  }
  return info;
}

/**
 * Extract mono PCM16 samples — downmixing stereo, converting
 * i8/i16/i24/i32/f32. Returns raw samples (not §6 framed).
 */
export function wavToMonoPcm16(bytes: Uint8Array): { samples: Int16Array; sampleRateHz: number } {
  const info = readWavHeader(bytes);
  const data = bytes.subarray(info.dataOffset, info.dataOffset + info.dataBytes);
  const bytesPerSample = info.bitsPerSample / 8;
  const frameCount = Math.floor(data.length / (bytesPerSample * info.channels));
  const mono = new Int16Array(frameCount);
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  for (let i = 0; i < frameCount; i++) {
    let sum = 0;
    for (let c = 0; c < info.channels; c++) {
      const offset = (i * info.channels + c) * bytesPerSample;
      let value: number;
      switch (info.bitsPerSample) {
        case 8:
          value = (view.getUint8(offset) - 128) / 128;
          break;
        case 16:
          value = view.getInt16(offset, true) / 32768;
          break;
        case 24: {
          const b0 = view.getUint8(offset);
          const b1 = view.getUint8(offset + 1);
          const b2 = view.getUint8(offset + 2);
          let raw = (b2 << 16) | (b1 << 8) | b0;
          if (raw & 0x800000) raw |= ~0xffffff;
          value = raw / 8388608;
          break;
        }
        case 32:
          value = info.isFloat ? view.getFloat32(offset, true) : view.getInt32(offset, true) / 2147483648;
          break;
        default:
          throw new Error(`unsupported bits/sample ${info.bitsPerSample}`);
      }
      if (!Number.isFinite(value)) value = 0;
      sum += value;
    }
    const average = sum / info.channels;
    const scaled = Math.round(average * 32768);
    mono[i] = scaled < -32768 ? -32768 : scaled > 32767 ? 32767 : scaled;
  }
  return { samples: mono, sampleRateHz: info.sampleRateHz };
}

function readU16LE(bytes: Uint8Array, offset: number): number {
  return bytes[offset] | (bytes[offset + 1] << 8);
}

function readU32LE(bytes: Uint8Array, offset: number): number {
  return (
    bytes[offset] | (bytes[offset + 1] << 8) | (bytes[offset + 2] << 16) | (bytes[offset + 3] << 24)
  );
}
