/**
 * Transport abstraction for Kodama UI.
 *
 * Components subscribe to typed events via `transport.on(event, cb)`.
 * The WebSocket implementation connects to the server's `/ws` endpoint
 * and demuxes binary frames into typed events.
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

export type Unsubscribe = () => void;

export type TransportEvents = {
  'video-init': VideoInitEvent;
  'video-segment': VideoSegmentEvent;
  'audio-data': AudioDataEvent;
  'audio-level': AudioLevelEvent;
  'telemetry': TelemetryEvent;
  'camera-event': CameraEvent;
};

export type TransportEventName = keyof TransportEvents;

export interface KodamaTransport {
  readonly connected: boolean;

  connect(url?: string): Promise<void>;
  disconnect(): void;

  on<E extends TransportEventName>(
    event: E,
    cb: (data: TransportEvents[E]) => void,
  ): Unsubscribe;

  listCameras(): Promise<CameraInfo[]>;
  getStatus(): Promise<ServerStatus>;
}
