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
