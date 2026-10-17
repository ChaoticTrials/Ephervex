import type {ScreenshotCallout} from './ThemeScreenshot';

export const createInstanceCallouts: ScreenshotCallout[] = [
  {number: 1, x: 4.2, y: 49.6, width: 84.5, height: 7.2},
  {number: 2, x: 4.2, y: 74.6, width: 91.7, height: 6.8},
  {number: 3, x: 4.2, y: 82.5, width: 91.7, height: 6.5},
];

export const manualDownloadCallouts: ScreenshotCallout[] = [
  {number: 1, x: 59.2, y: 13.8, width: 22.8, height: 62.4},
];

export const isolationAnswerCallouts: ScreenshotCallout[] = [
  {number: 1, x: 14.2, y: 27.5, width: 36.5, height: 45},
  {number: 2, x: 53.8, y: 27.5, width: 37.7, height: 45, badgeSide: 'right'},
];

export const diagnosticResultCallouts: ScreenshotCallout[] = [
  {number: 1, x: 1.6, y: 7, width: 96.8, height: 61},
  {number: 2, x: 1.6, y: 68, width: 10, height: 22},
];

export const serverAccessCallouts: ScreenshotCallout[] = [
  {number: 1, x: 5.5, y: 16, width: 89, height: 67},
];

export const serverOnlineModeCallouts: ScreenshotCallout[] = [
  {number: 1, x: 1, y: 8, width: 34, height: 84},
];

export const serverEulaCallouts: ScreenshotCallout[] = [
  {number: 1, x: 86, y: 22, width: 13, height: 54},
];
