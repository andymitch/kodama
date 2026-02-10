/**
 * WebSocket transport for Kodama UI.
 *
 * Connects to the server's `/ws` endpoint and demuxes binary messages
 * into typed events that components subscribe to.
 *
 * Binary protocol (matches server's web::ws module):
 *   0x01 + JSON            → camera list update (mapped to camera-event)
 *   0x02 + source_id(8) + codec_len(1) + codec + width(2) + height(2) + init_segment
 *                          → video init (fMP4 ftyp+moov)
 *   0x03 + source_id(8) + media_segment
 *                          → video segment (fMP4 moof+mdat)
 *   0x04 + source_id(8) + JSON
 *                          → telemetry update
 *   0x05 + source_id(8) + level_db(f32)
 *                          → audio level
 *   0x06 + source_id(8) + sample_rate(4) + channels(1) + pcm_data
 *                          → audio data
 */

import type {
  CameraEvent,
  CameraInfo,
  VideoInitEvent,
  VideoSegmentEvent,
  AudioDataEvent,
  AudioLevelEvent,
  TelemetryEvent,
  ServerStatus,
} from './types.js';
import type { KodamaTransport, TransportEventName, TransportEvents, Unsubscribe } from './transport.js';

const MSG_CAMERA_LIST = 0x01;
const MSG_VIDEO_INIT = 0x02;
const MSG_VIDEO_SEGMENT = 0x03;
const MSG_TELEMETRY = 0x04;
const MSG_AUDIO_LEVEL = 0x05;
const MSG_AUDIO_DATA = 0x06;

type Listener<E extends TransportEventName = TransportEventName> = (data: TransportEvents[E]) => void;

export class WebSocketTransport implements KodamaTransport {
  private ws: WebSocket | null = null;
  private listeners = new Map<TransportEventName, Set<Listener<any>>>();
  private baseUrl = '';
  private _connected = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelay = 1000;

  get connected(): boolean {
    return this._connected;
  }

  async connect(url?: string): Promise<void> {
    if (this.ws) {
      this.ws.close();
    }

    // Derive WebSocket URL from current page if not provided
    if (url) {
      this.baseUrl = url.replace(/^ws/, 'http').replace(/\/ws\/?$/, '');
    } else if (typeof window !== 'undefined') {
      if (window.location.protocol.startsWith('http')) {
        // Standard browser or dev server
        const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        url = `${proto}//${window.location.host}/ws`;
        this.baseUrl = window.location.origin;
      } else {
        // Non-HTTP protocol (e.g. tauri://) — connect to embedded server
        url = 'ws://localhost:3000/ws';
        this.baseUrl = 'http://localhost:3000';
      }
    } else {
      throw new Error('No URL provided and not running in browser');
    }

    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url!);
      ws.binaryType = 'arraybuffer';

      ws.onopen = () => {
        this._connected = true;
        this.reconnectDelay = 1000;
        resolve();
      };

      ws.onerror = (e) => {
        if (!this._connected) {
          reject(new Error('WebSocket connection failed'));
        }
      };

      ws.onclose = () => {
        this._connected = false;
        this.ws = null;
        this.scheduleReconnect(url!);
      };

      ws.onmessage = (event) => {
        if (event.data instanceof ArrayBuffer) {
          this.handleBinaryMessage(event.data);
        } else if (typeof event.data === 'string') {
          this.handleTextMessage(event.data);
        }
      };

      this.ws = ws;
    });
  }

  disconnect(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      this.ws.onclose = null; // Prevent reconnect
      this.ws.close();
      this.ws = null;
    }
    this._connected = false;
  }

  on<E extends TransportEventName>(
    event: E,
    cb: (data: TransportEvents[E]) => void,
  ): Unsubscribe {
    if (!this.listeners.has(event)) {
      this.listeners.set(event, new Set());
    }
    const set = this.listeners.get(event)!;
    set.add(cb as Listener<any>);
    return () => set.delete(cb as Listener<any>);
  }

  async listCameras(): Promise<CameraInfo[]> {
    const res = await fetch(`${this.baseUrl}/api/cameras`);
    if (!res.ok) throw new Error(`Failed to list cameras: ${res.status}`);
    return res.json();
  }

  async getStatus(): Promise<ServerStatus> {
    const res = await fetch(`${this.baseUrl}/api/status`);
    if (!res.ok) throw new Error(`Failed to get status: ${res.status}`);
    return res.json();
  }

  private emit<E extends TransportEventName>(event: E, data: TransportEvents[E]): void {
    const set = this.listeners.get(event);
    if (set) {
      for (const cb of set) {
        try {
          cb(data);
        } catch (e) {
          console.error(`[Transport] Error in ${event} listener:`, e);
        }
      }
    }
  }

  private handleTextMessage(data: string): void {
    // Text messages are JSON-encoded events (fallback for simple messages)
    try {
      const msg = JSON.parse(data);
      if (msg.type && msg.payload) {
        this.emit(msg.type as TransportEventName, msg.payload);
      }
    } catch {
      // Ignore unparseable text messages
    }
  }

  private handleBinaryMessage(buf: ArrayBuffer): void {
    const view = new DataView(buf);
    if (buf.byteLength < 1) return;

    const msgType = view.getUint8(0);

    switch (msgType) {
      case MSG_CAMERA_LIST:
        this.parseCameraList(buf);
        break;
      case MSG_VIDEO_INIT:
        this.parseVideoInit(buf, view);
        break;
      case MSG_VIDEO_SEGMENT:
        this.parseVideoSegment(buf);
        break;
      case MSG_TELEMETRY:
        this.parseTelemetry(buf);
        break;
      case MSG_AUDIO_LEVEL:
        this.parseAudioLevel(view);
        break;
      case MSG_AUDIO_DATA:
        this.parseAudioData(buf, view);
        break;
    }
  }

  private decodeSourceId(buf: ArrayBuffer, offset: number): string {
    const bytes = new Uint8Array(buf, offset, 8);
    return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
  }

  private parseCameraList(buf: ArrayBuffer): void {
    const json = new TextDecoder().decode(new Uint8Array(buf, 1));
    try {
      const cameras: CameraInfo[] = JSON.parse(json);
      // Emit individual camera-event for each camera
      for (const cam of cameras) {
        this.emit('camera-event', { source_id: cam.id, connected: cam.connected });
      }
    } catch (e) {
      console.error('[Transport] Failed to parse camera list:', e);
    }
  }

  private parseVideoInit(buf: ArrayBuffer, view: DataView): void {
    // 1(type) + 8(source_id) + 1(codec_len) + codec + 2(width) + 2(height) + init_segment
    if (buf.byteLength < 14) return;
    const sourceId = this.decodeSourceId(buf, 1);
    const codecLen = view.getUint8(9);
    const codec = new TextDecoder().decode(new Uint8Array(buf, 10, codecLen));
    const offset = 10 + codecLen;
    const width = view.getUint16(offset, true);
    const height = view.getUint16(offset + 2, true);
    const initSegment = buf.slice(offset + 4);

    this.emit('video-init', {
      source_id: sourceId,
      codec,
      width,
      height,
      init_segment: initSegment,
    });
  }

  private parseVideoSegment(buf: ArrayBuffer): void {
    // 1(type) + 8(source_id) + segment_data
    if (buf.byteLength < 9) return;
    const sourceId = this.decodeSourceId(buf, 1);
    this.emit('video-segment', {
      source_id: sourceId,
      data: buf.slice(9),
    });
  }

  private parseTelemetry(buf: ArrayBuffer): void {
    // 1(type) + 8(source_id) + JSON
    if (buf.byteLength < 9) return;
    const sourceId = this.decodeSourceId(buf, 1);
    const json = new TextDecoder().decode(new Uint8Array(buf, 9));
    try {
      const data = JSON.parse(json);
      this.emit('telemetry', { source_id: sourceId, ...data });
    } catch (e) {
      console.error('[Transport] Failed to parse telemetry:', e);
    }
  }

  private parseAudioLevel(view: DataView): void {
    // 1(type) + 8(source_id) + 4(f32 level_db)
    if (view.byteLength < 13) return;
    const sourceId = this.decodeSourceId(view.buffer, 1);
    const levelDb = view.getFloat32(9, true);
    this.emit('audio-level', { source_id: sourceId, level_db: levelDb });
  }

  private parseAudioData(buf: ArrayBuffer, view: DataView): void {
    // 1(type) + 8(source_id) + 4(sample_rate) + 1(channels) + pcm_data
    if (buf.byteLength < 14) return;
    const sourceId = this.decodeSourceId(buf, 1);
    const sampleRate = view.getUint32(9, true);
    const channels = view.getUint8(13);
    const data = buf.slice(14);
    this.emit('audio-data', { source_id: sourceId, data, sample_rate: sampleRate, channels });
  }

  private scheduleReconnect(url: string): void {
    if (this.reconnectTimer) return;
    this.reconnectTimer = setTimeout(async () => {
      this.reconnectTimer = null;
      try {
        await this.connect(url);
      } catch {
        this.reconnectDelay = Math.min(this.reconnectDelay * 2, 16000);
        this.scheduleReconnect(url);
      }
    }, this.reconnectDelay);
  }
}

/** Singleton transport instance */
let instance: KodamaTransport | null = null;

export function getTransport(): KodamaTransport {
  if (!instance) {
    instance = new WebSocketTransport();
  }
  return instance;
}
