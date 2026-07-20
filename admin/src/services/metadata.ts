import { request } from '@/services/http';
import type {
  AdminMediaItemQueryResult,
  MetadataItemType,
  ProviderIds,
  RemoteSearchPayload,
  RemoteSearchResult
} from '@/types/metadata';

export function searchItems(token: string, searchTerm: string, itemTypes: MetadataItemType[] = ['Movie', 'Series']) {
  const params = new URLSearchParams({
    Recursive: 'true',
    Limit: '50',
    SortBy: 'SortName'
  });
  if (searchTerm.trim()) {
    params.set('SearchTerm', searchTerm.trim());
  }
  if (itemTypes.length) {
    params.set('IncludeItemTypes', itemTypes.join(','));
  }

  return request<AdminMediaItemQueryResult>(`/Items?${params.toString()}`, { token });
}

export function resetMetadata(token: string, ids: string[]) {
  return request<void>('/items/metadata/reset', {
    method: 'POST',
    token,
    body: {
      Ids: ids
    }
  });
}

export function remoteSearch(token: string, payload: RemoteSearchPayload) {
  return request<RemoteSearchResult[]>(`/Items/RemoteSearch/${payload.itemType}`, {
    method: 'POST',
    token,
    body: {
      SearchInfo: {
        Name: payload.name,
        Year: payload.year ?? null,
        ProviderIds: compactProviderIds(payload.providerIds ?? {})
      }
    }
  });
}

export function applyRemoteSearch(token: string, itemId: string, result: RemoteSearchResult) {
  return request<void>(`/Items/RemoteSearch/Apply/${encodeURIComponent(itemId)}`, {
    method: 'POST',
    token,
    body: result as Record<string, unknown>
  });
}

function compactProviderIds(providerIds: ProviderIds) {
  return Object.entries(providerIds).reduce<Record<string, string | number>>((result, [provider, value]) => {
    if (value === null || value === undefined || value === '') {
      return result;
    }
    result[provider] = value;
    return result;
  }, {});
}
