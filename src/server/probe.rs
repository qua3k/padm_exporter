use log::error;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config;
use crate::padm_client::{
    client::PADMClient,
    device::{load_all_from, Device},
};

#[derive(Debug, Clone)]
struct Metric {
    name: String,
    mtype: String,
    help: String,
    metrics: Vec<DeviceMetric>,
}

#[derive(Debug, Clone)]
struct DeviceMetric {
    device: String,
    value: String,
    labels: HashMap<String, String>,
}

async fn get_devices_from(client: &PADMClient) -> Result<Vec<Device>, anyhow::Error> {
    let response = client.do_get("/api/variables").await;
    match response {
        Err(e) => Err(e.into()),
        Ok(r) => match r.error_for_status() {
            Err(e) => Err(e.into()),
            Ok(r) => match r.text().await {
                Err(e) => Err(e.into()),
                Ok(s) => {
                    let json = serde_json::from_str(&s);
                    let devices = load_all_from(&json?);
                    match devices {
                        Ok(v) => Ok(v),
                        Err(e) => Err(e.into()),
                    }
                }
            },
        },
    }
}

fn format_output_from_devices(devices: &Vec<Device>) -> Result<String, std::io::Error> {
    let mut body: String = String::new();
    let mut all_metrics: Vec<Metric> = Vec::new();

    for device in devices {
        for variable in &device.variables {
            let name = variable.get("name").to_string();
            let value = variable.get("value").to_string();

            if let Some(metric) = all_metrics.iter_mut().find(|x| x.name == name) {
                metric.metrics.push(DeviceMetric {
                    device: device.name.to_owned(),
                    value,
                    labels: match variable.labels() {
                        Some(l) => l.to_owned(),
                        None => HashMap::new(),
                    },
                });
            } else {
                let metric = Metric {
                    name,
                    mtype: variable.get("type").to_string(),
                    help: variable.get("help").to_string(),
                    metrics: vec![DeviceMetric {
                        device: device.name.to_owned(),
                        value,
                        labels: match variable.labels() {
                            Some(l) => l.to_owned(),
                            None => HashMap::new(),
                        },
                    }],
                };

                all_metrics.push(metric);
            }
        }
    }

    for metric in all_metrics {
        body.push_str(format!("# HELP {} {}\n", metric.name, metric.help).as_str());
        body.push_str(format!("# TYPE {} {}\n", metric.name, metric.mtype).as_str());

        for device_metric in metric.metrics {
            let mut inner: String = format!("device=\"{}\"", device_metric.device);
            for label in device_metric.labels {
                let (k, v) = label;
                inner = format!("{},{}=\"{}\"", inner, k, v);
            }
            body.push_str(
                format!(
                    "padm_{}{{{}}} {}\n",
                    metric.name, inner, device_metric.value,
                )
                .as_str(),
            );
        }
    }
    Ok(body)
}

pub async fn run(config: config::Config, body: Arc<Mutex<String>>) {
    let (tx, rx): (mpsc::Sender<()>, mpsc::Receiver<()>) = mpsc::channel();

    let devices: Arc<Mutex<Vec<Device>>> =
        Arc::new(Mutex::new(Vec::with_capacity(config.endpoints().len())));

    // Spawn client threads
    for endpoint in config.endpoints() {
        let client = PADMClient::new(
            endpoint.host().as_str(),
            endpoint.scheme(),
            endpoint.tls_insecure(),
            endpoint.interval(),
            endpoint.username(),
            endpoint.password(),
        );

        let devices = Arc::clone(&devices);
        let thread_tx = tx.clone();
        tokio::task::spawn(client_run(client, devices, thread_tx));
    }

    loop {
        // Blocks until data
        rx.recv().unwrap();

        let devices = devices.lock().unwrap();
        match format_output_from_devices(&devices) {
            Ok(output) => *body.lock().unwrap() = output,
            Err(e) => error!("Failed formatting metrics output: {}", e),
        }
    }
}

async fn client_run(
    client: PADMClient,
    devices: Arc<Mutex<Vec<Device>>>,
    thread_tx: mpsc::Sender<()>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(client.interval()));
    loop {
        interval.tick().await;
        match get_devices_from(&client).await {
            Ok(d) => *devices.lock().unwrap() = d,
            Err(e) => error!(
                "Failed getting devices from client {}: {}",
                &client.host(),
                e
            ),
        }
        thread_tx.send(()).unwrap()
    }
}
