import { request } from '@/services/http';
import type { CreateVirtualFolderPayload, LibraryPathPayload, VirtualFolder } from '@/types/library';

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

export function refreshLibrary(token: string) {
  return request<{ Scanning: boolean }>('/Library/Refresh', {
    method: 'POST',
    token
  });
}
