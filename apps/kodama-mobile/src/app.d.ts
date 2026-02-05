// See https://kit.svelte.dev/docs/types#app
declare global {
  namespace App {
    // interface Error {}
    // interface Locals {}
    // interface PageData {}
    // interface PageState {}
    // interface Platform {}
  }

  interface Window {
    __TAURI__?: {
      core: {
        invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
      };
    };
  }
}

export {};
