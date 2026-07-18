export type VirtualFolder = {
  Name: string;
  Id: string;
  ItemId: string;
  CollectionType: string;
  Locations: string[];
};

export type CreateVirtualFolderPayload = {
  name: string;
  collectionType: string;
  paths: string[];
};

export type LibraryPathPayload = {
  name: string;
  path: string;
};

export type DirectoryEntry = {
  Name: string;
  Path: string;
  Type: 'Directory' | 'File';
};

export type DefaultDirectoryBrowser = {
  Path?: string | null;
};
