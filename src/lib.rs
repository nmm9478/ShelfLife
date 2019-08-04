extern crate mongodb;
extern crate reqwest;
#[macro_use]
extern crate prettytable;

pub mod protocol;

use mongodb::db::ThreadedDatabase;
use mongodb::{bson, doc, Bson, ThreadedClient};
use prettytable::Table;
use protocol::*;
use reqwest::StatusCode;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Make a call to the Openshift API about some namespace info.
pub fn call_api(
    http_client: &reqwest::Client,
    call: &str,
    token: &str,
) -> Result<reqwest::Response> {
    let response = http_client 
        .get(call)
        .header("Authorization", format!("Bearer {}", token))
        .send()?;

    // Ensure the call was successful
    if response.status() == StatusCode::OK {
        Ok(response)
    } else {
        return Err(From::from(format!(
            "Error! Could not run API call. Call: {}, Code: {}", call, response.status()),
        ));
    }
}

pub fn query_known_namespace(
    mongo_client: &mongodb::Client,
    collection: &str,
    http_client: &reqwest::Client,
    token: &str,
    endpoint: &str,
    namespace: &str,
) -> Result<()> {
    println!(
        "{}",
        format!("\nQuerying API for namespace {}...", namespace).to_string()
    );
    let namespace_info = get_shelflife_info(
        http_client,
        token,
        endpoint,
        namespace,
    )?;
    print!("\n > > > API Response > > > ");
    println!(
        "{} {:?} {} {}",
        namespace_info.name, namespace_info.admins, namespace_info.last_update, namespace_info.cause
    );

    // Query the DB and get back a table of already added namespaces
    let current_table: Vec<DBItem> = get_db(mongo_client, &collection)?;
    
    // Check if the namespace queried for is in the DB, and if not, ask to put it in.
    let queried_namespace = namespace_info.name.to_string();
    if !current_table
        .iter()
        .any(|x| x.name.to_string() == queried_namespace)
    {
        println!(
            "\nThis namespace ({}) is not in the database! Would you like to add it? (y/n): ",
            queried_namespace
        );
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .expect("Could not read response");
        if input.trim() == "y" {
             match collection.as_ref() {
                "namespaces" => {
                    println!("Putting a ShelfLife on {}", queried_namespace);
                }
                "whitelist" => {
                    println!("Whitelisting {}", queried_namespace);
                }
                _ => {
                    println!("\nUnknown table:");
                }
            }
            let _table_add = add_item_to_db(mongo_client, &collection, namespace_info);
        } else if input.trim() == "n" {
            println!("Ok.");
        } else {
            println!("Invalid response.");
        }
    } else {
        println!("The requested namespace is in the database.")
    }
    Ok(())
}


// Queries the API and returns a Struct with data relevant for shelflife's operation.
fn get_shelflife_info(
    http_client: &reqwest::Client,
    token: &str,
    endpoint: &str,
    namespace: &str,
) -> Result<DBItem> {
    // Query for builds
    // Formulate the call

    let buildlist_call = format!(
        "https://{}/apis/build.openshift.io/v1/namespaces/{}/builds",
        endpoint, namespace
    );
    let buildlist_resp = call_api(&http_client, &buildlist_call, &token); // Make the call
    let buildlist_json: BuildlistResponse = buildlist_resp?.json()?; // Bind json of reply to struct.
    let mut last_builds = Vec::new();
    for item in buildlist_json.items {
        last_builds.push(item.status.completion_timestamp);
    }

    // Query for deployment configs
    // Formulate the call
    let deploycfgs_call = format!(
        "https://{}/apis/apps.openshift.io/v1/namespaces/{}/deploymentconfigs",
        endpoint, namespace
    );
    let deploycfgs_resp = call_api(&http_client, &deploycfgs_call, &token); // Make the call
    let deploycfgs_json: DeploymentResponse = deploycfgs_resp?.json()?; // Bind json of reply to struct.
    // Get the timestamp of the last deployments.
    let mut last_deploys = Vec::new();
    for config in deploycfgs_json.items {
        for condition in config.status.conditions {
            last_deploys.push(condition.last_update_time);
        }
    }
    
    // Query for rolebindings (for the admins of the namespace)
    let rolebdgs_call = format!(
        "https://{}/apis/authorization.openshift.io/v1/namespaces/{}/rolebindings",
        endpoint, namespace
    );
    let rolebdgs_resp = call_api(&http_client, &rolebdgs_call, &token);
    let rolebdgs_json: RolebindingsResponse = rolebdgs_resp?.json()?;
    let rolebdgs: Vec<String> = rolebdgs_json
        .items
        .into_iter()
        .filter(|item| item.metadata.name == "admin")
        .filter_map(|item| item.user_names)
        .flatten()
        .collect();

    // Build the response struct
    let api_response = DBItem {
        name: namespace.to_string(),
        admins: rolebdgs,
        last_update:
            match last_deploys.first() {
                Some(ref x) => x.to_string(),
                _ => "N/A".to_string(),
            },
        cause: "Deployment".to_string(), 
    };
    Ok(api_response)
}


fn get_db(mongo_client: &mongodb::Client, collection: &str) -> Result<Vec<DBItem>> {
    let coll = mongo_client
        .db("SHELFLIFE_NAMESPACES")
        .collection(&collection);
    let mut namespace_table = Vec::new(); // The vec of namespace information we're gonna send back.

    // Find the document and receive a cursor
    let cursor = coll.find(None, None).unwrap();
    for result in cursor {
        if let Ok(item) = result {
            let mut doc_name = String::new();
            let mut doc_admins: Vec<String> = Vec::new();
            let mut doc_last_deployment = String::new();
            if let Some(&Bson::String(ref name)) = item.get("name") {
                doc_name = name.to_string();
            }
            if let Some(&Bson::Array(ref admins)) = item.get("admins") {
                let doc_admins_bson = admins.to_vec();
                for item in doc_admins_bson {
                    doc_admins.push(item.to_string());
                }
            }
            if let Some(&Bson::String(ref last_deployment)) = item.get("last_deployment") {
                doc_last_deployment = last_deployment.to_string();
            }
            let namespace_document = DBItem {
                name: doc_name.as_str().to_string(),
                admins: doc_admins,
                last_update: doc_last_deployment,
                cause: "Deployment".to_string(),
            };
            namespace_table.push(namespace_document);
        }
    }
    Ok(namespace_table)
}

pub fn view_db(mongo_client: &mongodb::Client, collection: &str) -> Result<()> {
    // Query the DB and get back a table of already added namespaces
    let current_table: Vec<DBItem> = get_db(mongo_client, collection)?;
    match collection.as_ref() {
        "namespaces" => {
            println!("\nProjects with ShelfLives:");
        }
        "whitelist" => {
            println!("\nWhitelisted projects:");
        }
        _ => {
            println!("\nUnknown table:");
        }
    }
    let mut db_table = Table::new(); // Create the table
    db_table.add_row(row!["Namespace", "Admins", "Latest Update", "Cause"]); // Add a row per time
    for row in &current_table {
        db_table.add_row(row![
            row.name,
            format!("{:?}", row.admins),
            row.last_update,
            row.cause,
        ]);
    }
    db_table.printstd(); // Print the table to stdout
    Ok(())
}


fn add_item_to_db(mongo_client: &mongodb::Client, collection: &str, item: DBItem) -> Result<()> {
    // Direct connection to a server. Will not look for other servers in the topology.
    let coll = mongo_client
        .db("SHELFLIFE_NAMESPACES")
        .collection(&collection);
    coll.insert_one(doc!{"name": item.name, "admins": bson::to_bson(&item.admins)?, "last_update": item.last_update, "cause": item.cause}, None).unwrap();
    Ok(())
}


pub fn remove_db_item(mongo_client: &mongodb::Client, collection: &str, namespace: &str) -> Result<()> {
    // Direct connection to a server. Will not look for other servers in the topology.
    let coll = mongo_client
        .db("SHELFLIFE_NAMESPACES")
        .collection(collection);
    coll.find_one_and_delete(doc! {"name": namespace}, None)
        .unwrap();
    println!("{} has been removed.", namespace);
    Ok(())
}
