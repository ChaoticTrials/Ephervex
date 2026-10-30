import type {ScreenshotCallout} from './ThemeScreenshot';

export const createInstanceCallouts: ScreenshotCallout[] = [
  {number: 1, x: 2.6, y: 25.2, width: 67.2, height: 5.6},
  {number: 2, x: 2.6, y: 34.4, width: 71.2, height: 5.4},
  {number: 3, x: 74.6, y: 34.4, width: 21.6, height: 5.4},
];

export const manualDownloadCallouts: ScreenshotCallout[] = [
  {number: 1, x: 59.2, y: 13.8, width: 22.8, height: 62.4},
];

export const isolationAnswerCallouts: ScreenshotCallout[] = [
  {number: 1, x: 2.2, y: 14, width: 46, height: 68},
  {number: 2, x: 49.5, y: 14, width: 45.5, height: 68, badgeSide: 'right'},
];

export const diagnosticResultCallouts: ScreenshotCallout[] = [
  {number: 1, x: 1.6, y: 3, width: 96.8, height: 62},
  {number: 2, x: 3, y: 65, width: 21.6, height: 24},
];

export const serverAccessCallouts: ScreenshotCallout[] = [
  {number: 1, x: 5.5, y: 21, width: 89, height: 59},
];

export const serverOnlineModeCallouts: ScreenshotCallout[] = [
  {number: 1, x: 29, y: 16, width: 9, height: 58},
];

export const serverEulaCallouts: ScreenshotCallout[] = [
  {number: 1, x: 86, y: 22, width: 13, height: 54},
];
