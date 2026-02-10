export interface CameraInfo {
  id: string;
  name: string;
  connected: boolean;
}

export interface VideoInitEvent {
  source_id: string;
  codec: string;
  width: number;
  height: number;
  init_segment: ArrayBuffer;
}

export interface VideoSegmentEvent {
  source_id: string;
  data: ArrayBuffer;
}

export interface AudioLevelEvent {
  source_id: string;
  level_db: number;
}

export interface AudioDataEvent {
  source_id: string;
  data: ArrayBuffer;
  sample_rate: number;
  channels: number;
}

export interface GpsData {
  latitude: number;
  longitude: number;
  altitude: number | null;
  speed: number | null;
  heading: number | null;
  fix_mode: number;
}

export interface TelemetryEvent {
  source_id: string;
  cpu_usage: number;
  cpu_temp: number | null;
  memory_usage: number;
  disk_usage: number;
  uptime_secs: number;
  load_average: [number, number, number];
  gps: GpsData | null;
  motion_level: number | null;
}

export interface CameraEvent {
  source_id: string;
  connected: boolean;
}

export interface ServerStatus {
  cameras: number;
  clients: number;
  uptime_secs: number;
  frames_received: number;
  frames_broadcast: number;
  public_key?: string;
}
