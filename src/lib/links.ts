export const REPO_URL = "https://github.com/asthetik/video-fetch";
export const AUTHOR_URL = "https://github.com/asthetik";
export const LICENSE_URL = `${REPO_URL}/blob/main/LICENSE`;
export const THIRD_PARTY_URL = `${REPO_URL}/blob/main/THIRD_PARTY.md`;

export function releaseUrl(version: string): string {
  return `${REPO_URL}/releases/tag/v${version}`;
}
