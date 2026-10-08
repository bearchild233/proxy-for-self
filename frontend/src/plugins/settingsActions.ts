export interface SettingsActions { changed: boolean, saving: boolean, saveDisabled: boolean, resetDisabled: boolean }

export function settingsActions(input: Record<string, unknown>): SettingsActions {
  return { changed: input.changed === true, saving: input.saving === true, saveDisabled: input.saveDisabled !== false, resetDisabled: input.resetDisabled !== false }
}
