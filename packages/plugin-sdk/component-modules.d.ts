declare module 'proxy:bridge' {
  const bridge: import('./ui').PluginBridge & { events: EventTarget }
  export default bridge
}
