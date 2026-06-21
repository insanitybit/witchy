// Navigation between the read-only views.
export interface Nav {
  index(): void;
  rune(name: string): void;
  version(name: string, version: string): void;
  trust(): void;
}
