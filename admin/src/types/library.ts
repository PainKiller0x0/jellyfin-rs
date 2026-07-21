export type VirtualFolder = {
  Name: string;
  Id: string;
  ItemId: string;
  CollectionType: string;
  Locations: string[];
  LibraryOptions: LibraryOptions;
};

export type LibraryTypeOptions = {
  Type: string;
  MetadataFetchers?: string[];
  ImageFetchers?: string[];
  [key: string]: unknown;
};

export type LibraryOptions = {
  Enabled?: boolean;
  EnablePhotos?: boolean;
  EnableRealtimeMonitor?: boolean;
  EnableAutomaticSeriesGrouping?: boolean;
  PreferredMetadataLanguage?: string | null;
  MetadataCountryCode?: string | null;
  SaveLocalMetadata?: boolean;
  AutomaticallyAddToCollection?: boolean;
  EnableChapterImageExtraction?: boolean;
  ExtractChapterImagesDuringLibraryScan?: boolean;
  EnableTrickplayImageExtraction?: boolean;
  ExtractTrickplayImagesDuringLibraryScan?: boolean;
  TypeOptions?: LibraryTypeOptions[];
  [key: string]: unknown;
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

export type UpdateLibraryPathPayload = LibraryPathPayload & {
  newPath: string;
};

export type DirectoryEntry = {
  Name: string;
  Path: string;
  Type: 'Directory' | 'File';
};

export type DefaultDirectoryBrowser = {
  Path?: string | null;
};
