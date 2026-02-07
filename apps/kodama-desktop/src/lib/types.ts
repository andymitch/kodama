/** Event payload types matching Rust event structs */

export interface CameraEvent {
	source_id: string;
	connected: boolean;
}

export interface VideoInitEvent {
	source_id: string;
	codec: string;
	width: number;
	height: number;
	init_segment: string; // base64
}

export interface VideoSegmentEvent {
	source_id: string;
	data: string; // base64
}

export interface AudioLevelEvent {
	source_id: string;
	level_db: number;
}

export interface AudioDataEvent {
	source_id: string;
	data: string; // base64-encoded PCM s16le
	sample_rate: number;
	channels: number;
}

export interface TelemetryEvent {
	source_id: string;
	cpu_usage: number;
	cpu_temp: number | null;
	memory_usage: number;
	disk_usage: number;
	uptime_secs: number;
	load_average: [number, number, number];
}
