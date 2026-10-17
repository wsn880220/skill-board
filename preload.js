const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('api', {
  onView: fn => ipcRenderer.on('view', (_e, v) => fn(v)),
  onStatus: fn => ipcRenderer.on('status', (_e, s) => fn(s)),
  rescan: () => ipcRenderer.invoke('rescan'),
  getSettings: () => ipcRenderer.invoke('get-settings'),
  saveSettings: s => ipcRenderer.invoke('save-settings', s),
  regenerate: id => ipcRenderer.invoke('regenerate', id),
  saveSkill: s => ipcRenderer.invoke('save-skill', s),
});
