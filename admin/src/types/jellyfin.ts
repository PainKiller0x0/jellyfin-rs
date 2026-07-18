export type JellyfinUserPolicy = {
  IsAdministrator?: boolean;
  IsDisabled?: boolean;
};

export type JellyfinUser = {
  Id: string;
  Name: string;
  ServerId?: string;
  HasPassword?: boolean;
  Policy?: JellyfinUserPolicy;
};

export type AuthenticationResult = {
  User: JellyfinUser;
  AccessToken: string;
  ServerId: string;
};

export type ApiErrorBody = {
  Error?: string;
  Message?: string;
};
