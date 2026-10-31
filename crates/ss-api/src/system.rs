//! `Info`, `License` and `HomeMode`.

use crate::client::{ApiRequest, Client};
use crate::error::Result;
use crate::models::{HomeModeInfo, LicenseInfo, SsInfo};

/// Surveillance Station's info API.
pub const INFO_API: &str = "SYNO.SurveillanceStation.Info";
/// The license API.
pub const LICENSE_API: &str = "SYNO.SurveillanceStation.License";
/// The home mode API.
pub const HOME_MODE_API: &str = "SYNO.SurveillanceStation.HomeMode";

fn csv<T: ToString>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

impl Client {
    /// `Info.GetInfo`, asking for up to version 8 (clamped to what the NAS
    /// offers): package version, serial, model, camera and license counts.
    pub async fn ss_info(&self) -> Result<SsInfo> {
        self.send_as(&ApiRequest::new(INFO_API, "GetInfo", 8)).await
    }

    /// `License.Load` v1.
    pub async fn load_licenses(&self) -> Result<LicenseInfo> {
        self.send_as(&ApiRequest::new(LICENSE_API, "Load", 1)).await
    }

    /// `License.AddKey` v1 (online activation through the NAS).
    pub async fn add_licenses(&self, keys: &[&str]) -> Result<()> {
        let req = ApiRequest::new(LICENSE_API, "AddKey", 1).param("licenseList", keys.join(","));
        self.send(&req).await.map(drop)
    }

    /// `License.DeleteKey` v1.
    pub async fn delete_licenses(&self, license_ids: &[i64]) -> Result<()> {
        let req = ApiRequest::new(LICENSE_API, "DeleteKey", 1).param("lic_list", csv(license_ids));
        self.send(&req).await.map(drop)
    }

    /// `HomeMode.GetInfo` v1.
    pub async fn home_mode(&self) -> Result<HomeModeInfo> {
        self.send_as(&ApiRequest::new(HOME_MODE_API, "GetInfo", 1))
            .await
    }

    /// `HomeMode.Switch` v1.
    pub async fn set_home_mode(&self, on: bool) -> Result<()> {
        let req = ApiRequest::new(HOME_MODE_API, "Switch", 1).param("on", on);
        self.send(&req).await.map(drop)
    }
}
