interface TcodeWebRuntimeConfig {
  apiBase?: string;
  routerBase?: string;
  eventSourceWithCredentials?: boolean;
}

declare global {
  interface Window {
    __TCODE_WEB_CONFIG__?: TcodeWebRuntimeConfig;
  }
}

export {};
