declare module "jsdom" {
  export class JSDOM {
    constructor(html?: string, options?: { url?: string; runScripts?: string });
    readonly window: Window & typeof globalThis;
  }
}
