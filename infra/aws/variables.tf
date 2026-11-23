variable "region" {
  description = "AWS region. ap-east-2 (Taipei) must be opted in on the account first."
  type        = string
  default     = "ap-east-2"
}

variable "instance_type" {
  description = "Graviton instance type for the single k3s node."
  type        = string
  default     = "m7g.large"
}

variable "root_volume_gb" {
  description = "Size of the encrypted gp3 root volume."
  type        = number
  default     = 40
}

variable "artifact_bucket_name" {
  description = "Globally unique name of the private artifact bucket."
  type        = string
}

variable "artifact_prefix" {
  description = "Key prefix inside the artifact bucket the node may read."
  type        = string
  default     = "artifacts/"
}

variable "ssm_parameter_prefix" {
  description = "SSM Parameter Store path prefix holding secrets. The node may read everything below it."
  type        = string
  default     = "/via/prod"
}

variable "route53_zone_id" {
  description = "Hosted zone id for the API record. Leave empty to skip DNS and use the Elastic IP output."
  type        = string
  default     = ""
}

variable "api_hostname" {
  description = "Fully qualified API hostname. Only used when route53_zone_id is set."
  type        = string
  default     = ""
}

variable "repo_url" {
  description = "Git URL the node clones to obtain the bootstrap scripts and overlay."
  type        = string
  default     = "https://github.com/chengpong1127/Via.git"
}

variable "repo_ref" {
  description = "Commit sha or tag the node checks out as root. Required; pin it, never a moving branch. Changing it does not re-run cloud-init on an existing node."
  type        = string

  validation {
    condition     = can(regex("^[A-Za-z0-9._/-]+$", var.repo_ref))
    error_message = "repo_ref must be a plain commit sha, tag or branch name."
  }
}

variable "data_volume_gb" {
  description = "Size of the separate encrypted gp3 data volume holding /var/lib/via and the k3s state."
  type        = number
  default     = 40
}

variable "snapshot_retention" {
  description = "Number of daily EBS snapshots to keep."
  type        = number
  default     = 7
}
