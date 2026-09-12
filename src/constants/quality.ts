export const DEFAULT_QUALITY = 'ORIGIN'

export const QUALITY_OPTIONS = [
  { label: '原画', value: 'ORIGIN' },
  { label: '蓝光', value: 'FULL_HD1' },
  { label: '超清', value: 'HD1' },
  { label: '高清', value: 'SD2' },
  { label: '标清', value: 'SD1' },
] as const

export function normalizeQuality(quality: string | null | undefined): string {
  const value = quality?.trim()
  return QUALITY_OPTIONS.find(option => option.value === value)?.value ?? DEFAULT_QUALITY
}
