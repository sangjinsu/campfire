//! Development-only account adapter, FIFO commands, and recipient-local inventory.
//! Run with CAMPFIRE_DEMO_ALICE_CREDENTIAL and CAMPFIRE_DEMO_BOB_CREDENTIAL set.
use campfire_runtime::{
    AccountAuthenticator, AccountGrant, AccountIdentity, AuthError, Campfire, Config, Game,
    GameContext, GameError, GameModule, PlayerIdentity,
};
use campfire_server::dashboard::{Dashboard, DashboardOptions};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap, error::Error, future::Future, net::SocketAddr, pin::Pin, sync::Arc,
    time::Duration,
};

// This exact-match adapter is only for local development. A production host must
// replace it with a verifier that checks its provider's signature and policies.
struct DevelopmentAccounts {
    alice: String,
    bob: String,
}
impl DevelopmentAccounts {
    fn from_environment() -> Result<Self, Box<dyn Error>> {
        let alice = std::env::var("CAMPFIRE_DEMO_ALICE_CREDENTIAL")?;
        let bob = std::env::var("CAMPFIRE_DEMO_BOB_CREDENTIAL")?;
        if alice.is_empty() || bob.is_empty() || alice == bob {
            return Err("development credentials must be nonempty and different".into());
        }
        Ok(Self { alice, bob })
    }
}
impl AccountAuthenticator for DevelopmentAccounts {
    fn authenticate<'a>(
        &'a self,
        game_id: &'a str,
        credential: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<AccountGrant, AuthError>> + Send + 'a>> {
        Box::pin(async move {
            if game_id != "inventory" {
                return Err(AuthError::InvalidCredential);
            }
            let subject = if credential == self.alice {
                "alice"
            } else if credential == self.bob {
                "bob"
            } else {
                return Err(AuthError::InvalidCredential);
            };
            Ok(AccountGrant {
                identity: AccountIdentity {
                    issuer: "campfire-development".into(),
                    subject: subject.into(),
                    claims: json!({"initial_secret": format!("{subject}-starter-item")}),
                },
                valid_for: Duration::from_secs(3600),
            })
        })
    }
}

struct InventoryModule;
#[derive(Default)]
struct InventoryGame {
    collected: u64,
    inventories: BTreeMap<String, Vec<String>>,
}
impl GameModule for InventoryModule {
    fn create(&self, _: &Value, _: &GameContext) -> Result<Box<dyn Game>, GameError> {
        Ok(Box::new(InventoryGame::default()))
    }
}
impl Game for InventoryGame {
    fn player_joined(&mut self, id: &str) {
        self.inventories.insert(id.into(), Vec::new());
    }
    fn player_joined_with_identity(&mut self, id: &str, identity: &PlayerIdentity) {
        self.player_joined(id);
        if let PlayerIdentity::Account(account) = identity
            && let Some(item) = account.claims["initial_secret"].as_str()
        {
            self.inventories.get_mut(id).unwrap().push(item.into());
        }
    }
    // Rotation preserves the existing inventory; claims are trusted host metadata.
    fn player_identity_changed(&mut self, _: &str, _: &PlayerIdentity) {}
    fn player_left(&mut self, id: &str) {
        self.inventories.remove(id);
    }
    fn player_connection_changed(&mut self, _: &str, _: bool) {}
    fn validate_input(&self, _: &str, _: &Value) -> Result<(), GameError> {
        Err(GameError::new(
            "INPUT_UNSUPPORTED",
            "use game.command to collect an item",
        ))
    }
    fn apply_input(&mut self, _: &str, _: &Value) {}
    fn validate_command(&self, id: &str, payload: &Value) -> Result<(), GameError> {
        if payload["action"] != "collect"
            || !matches!(payload["item"].as_str(), Some("wood" | "stone" | "key"))
        {
            return Err(GameError::new(
                "INVALID_ITEM",
                "collect wood, stone, or key",
            ));
        }
        if self
            .inventories
            .get(id)
            .is_none_or(|items| items.len() >= 32)
        {
            return Err(GameError::new(
                "INVENTORY_FULL",
                "inventory holds at most 32 items",
            ));
        }
        Ok(())
    }
    fn apply_command(&mut self, id: &str, payload: &Value) -> Result<Value, GameError> {
        // Validation and mutation run in the same room actor turn.
        let inventory = self
            .inventories
            .get_mut(id)
            .ok_or_else(|| GameError::new("PLAYER_MISSING", "player has no inventory"))?;
        inventory.push(payload["item"].as_str().unwrap().into());
        self.collected += 1;
        Ok(json!({"inventory_count":inventory.len()}))
    }
    fn private_snapshot(&self, id: &str) -> Option<Value> {
        self.inventories
            .get(id)
            .map(|inventory| json!({"inventory":inventory}))
    }
    fn tick(&mut self, _: Duration) {}
    fn snapshot(&self) -> Value {
        json!({"total_collected":self.collected,"players":self.inventories.len()})
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let accounts = DevelopmentAccounts::from_environment()?;
    let admin_options = DashboardOptions::from_env()?;
    let bind = std::env::var("CAMPFIRE_DEMO_BIND").unwrap_or_else(|_| "127.0.0.1:3001".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    let address = listener.local_addr()?;
    let runtime = Campfire::with_authenticator(Config::default(), Arc::new(accounts))?;
    if let Err(error) = runtime.register_game("inventory", InventoryModule) {
        runtime.shutdown().await;
        return Err(error.into());
    }
    let mut dashboard = match admin_options {
        Some(options) => match Dashboard::start(runtime.observer(), options) {
            Ok(dashboard) => Some(dashboard),
            Err(error) => {
                runtime.shutdown().await;
                return Err(error.into());
            }
        },
        None => None,
    };
    let app = dashboard.as_ref().map_or_else(
        || runtime.router(),
        |dashboard| runtime.router().merge(dashboard.router()),
    );
    eprintln!(
        "Campfire development inventory: http://{} (game_id=inventory)",
        address
    );
    let shutdown = runtime.clone();
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown.shutdown().await;
    })
    .await;
    if let Some(dashboard) = &mut dashboard {
        dashboard.shutdown().await;
    }
    runtime.shutdown().await;
    result?;
    Ok(())
}
