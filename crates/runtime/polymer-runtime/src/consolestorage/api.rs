use super::StorageService;
use polymer_deno::polymer::ConsoleStorage;

impl StorageService {
    pub fn client(&self) -> ConsoleStorage {
        ConsoleStorage {
            platforms: self.actor.clone().recipient(),
            inspect: self.actor.clone().recipient(),
            recover: self.actor.clone().recipient(),
            install: self.actor.clone().recipient(),
            bootstrap: self.actor.clone().recipient(),
            uninstall: self.actor.clone().recipient(),
            plan_removal: self.actor.clone().recipient(),
            list: self.actor.clone().recipient(),
            supports: self.actor.clone().recipient(),
            create: self.actor.clone().recipient(),
            delete: self.actor.clone().recipient(),
            set_label: self.actor.clone().recipient(),
        }
    }
}
