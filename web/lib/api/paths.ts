export const worldExportDownloadPath = (serverId: string, exportId: string) =>
  `/api/v1/servers/${encodeURIComponent(serverId)}/exports/${encodeURIComponent(exportId)}/download`;

export const serverEventStreamPath = (serverId: string) =>
  `/api/v1/servers/${encodeURIComponent(serverId)}/events`;
