/** Only reorder the visible IDs; preserve hidden rows and the latest row objects. */
export function applyRowOrder<Row extends { id: string }>(rows: Row[], orderedIds: readonly string[]): Row[] {
  const wanted = new Set(orderedIds)
  const visible = new Map(rows.filter(row => wanted.has(row.id)).map(row => [row.id, row]))
  if (wanted.size !== orderedIds.length || visible.size !== wanted.size)
    return rows
  let index = 0
  const next = rows.map(row => wanted.has(row.id) ? visible.get(orderedIds[index++]!)! : row)
  return next.every((row, position) => row === rows[position]) ? rows : next
}
