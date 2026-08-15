export class Store {
  save(): void {}
}

export function save(): void {}

export function persist(store: Store): void {
  save();
  store.save();
}
