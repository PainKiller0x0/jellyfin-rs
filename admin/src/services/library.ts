import { request, requestUrl } from '@/services/http';
import type {
  CreateVirtualFolderPayload,
  DefaultDirectoryBrowser,
  DirectoryEntry,
  LibraryOptions,
  LibraryPathPayload,
  UpdateLibraryPathPayload,
  VirtualFolder
} from '@/types/library';

export function virtualFolders(token: string) {
  return request<VirtualFolder[]>('/Library/VirtualFolders', { token });
}

export function createVirtualFolder(token: string, payload: CreateVirtualFolderPayload) {
  return request<void>('/Library/VirtualFolders', {
    method: 'POST',
    token,
    body: {
      Name: payload.name,
      CollectionType: payload.collectionType,
      Paths: payload.paths.join('|')
    }
  });
}

export function deleteVirtualFolder(token: string, name: string) {
  return request<void>(`/Library/VirtualFolders?Name=${encodeURIComponent(name)}`, {
    method: 'DELETE',
    token
  });
}

export function renameVirtualFolder(token: string, name: string, newName: string) {
  const params = new URLSearchParams({ Name: name, NewName: newName });
  return request<void>(`/Library/VirtualFolders/Name?${params.toString()}`, {
    method: 'POST',
    token
  });
}

export function updateLibraryOptions(token: string, id: string, options: LibraryOptions) {
  return request<void>('/Library/VirtualFolders/LibraryOptions', {
    method: 'POST',
    token,
    body: {
      Id: id,
      LibraryOptions: options
    }
  });
}

export function addLibraryPath(token: string, payload: LibraryPathPayload) {
  return request<void>('/Library/VirtualFolders/Paths', {
    method: 'POST',
    token,
    body: {
      Name: payload.name,
      Path: payload.path
    }
  });
}

export function deleteLibraryPath(token: string, payload: LibraryPathPayload) {
  return request<void>(
    `/Library/VirtualFolders/Paths?Name=${encodeURIComponent(payload.name)}&Path=${encodeURIComponent(payload.path)}`,
    {
      method: 'DELETE',
      token
    }
  );
}

export function updateLibraryPath(token: string, payload: UpdateLibraryPathPayload) {
  return request<void>('/Library/VirtualFolders/Paths/Update', {
    method: 'POST',
    token,
    body: {
      Name: payload.name,
      Path: payload.path,
      PathInfo: {
        Path: payload.newPath
      }
    }
  });
}

export function refreshLibrary(token: string, libraryId?: string) {
  const params = libraryId ? `?LibraryId=${encodeURIComponent(libraryId)}` : '';
  return request<{ Scanning: boolean; AlreadyRunning?: boolean }>(`/Library/Refresh${params}`, {
    method: 'POST',
    token
  });
}

export function uploadLibraryPrimaryImage(token: string, itemId: string, file: Blob) {
  return request<void>(`/Items/${encodeURIComponent(itemId)}/Images/Primary`, {
    method: 'POST',
    token,
    headers: {
      'Content-Type': file.type || 'application/octet-stream'
    },
    body: file
  });
}

export function deleteLibraryPrimaryImage(token: string, itemId: string) {
  return request<void>(`/Items/${encodeURIComponent(itemId)}/Images/Primary`, {
    method: 'DELETE',
    token
  });
}

export function libraryPrimaryImageUrl(itemId: string, token: string, version: number) {
  const params = new URLSearchParams({
    api_key: token,
    tag: String(version)
  });
  return requestUrl(`/Items/${encodeURIComponent(itemId)}/Images/Primary?${params.toString()}`);
}

export function defaultDirectoryBrowser(token: string) {
  return request<DefaultDirectoryBrowser>('/Environment/DefaultDirectoryBrowser', { token });
}

export function drives(token: string) {
  return request<DirectoryEntry[]>('/Environment/Drives', { token });
}

export function directoryContents(token: string, path: string) {
  return request<DirectoryEntry[]>(
    `/Environment/DirectoryContents?Path=${encodeURIComponent(path)}&IncludeDirectories=true&IncludeFiles=false`,
    { token }
  );
}

export function parentPath(token: string, path: string) {
  return request<string | null>(`/Environment/ParentPath?Path=${encodeURIComponent(path)}`, { token });
}

export function validateDirectoryPath(token: string, path: string) {
  return request<void>('/Environment/ValidatePath', {
    method: 'POST',
    token,
    body: {
      Path: path,
      IsFile: false
    }
  });
}
