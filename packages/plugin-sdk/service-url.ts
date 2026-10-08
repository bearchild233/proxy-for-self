import { context } from './ui'
export function resolveServiceRootUrl() { return context().apiBaseUrl.replace(/\/v1\/?$/, '') }
