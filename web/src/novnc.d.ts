declare module "@novnc/novnc" {
  export default class RFB extends EventTarget {
    constructor(target: HTMLElement, url: string | WebSocket, options?: { wsProtocols?: string[]; shared?: boolean });
    scaleViewport: boolean;
    resizeSession: boolean;
    focusOnClick: boolean;
    qualityLevel: number;
    compressionLevel: number;
    disconnect(): void;
    sendCtrlAltDel(): void;
    focus(): void;
  }
}
