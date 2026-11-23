data "aws_caller_identity" "current" {}

data "aws_iam_policy_document" "assume_ec2" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

data "aws_iam_policy_document" "node" {
  statement {
    sid       = "ReadArtifacts"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.artifacts.arn}/${var.artifact_prefix}*"]
  }

  statement {
    sid       = "ListArtifacts"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.artifacts.arn]

    condition {
      test     = "StringLike"
      variable = "s3:prefix"
      values   = ["${var.artifact_prefix}*"]
    }
  }

  statement {
    sid     = "ReadSecrets"
    actions = ["ssm:GetParameter", "ssm:GetParameters"]
    resources = [
      "arn:aws:ssm:${var.region}:${data.aws_caller_identity.current.account_id}:parameter${var.ssm_parameter_prefix}/*",
    ]
  }
}

resource "aws_iam_role" "node" {
  name               = "via-node"
  assume_role_policy = data.aws_iam_policy_document.assume_ec2.json
}

resource "aws_iam_role_policy" "node" {
  name   = "via-node"
  role   = aws_iam_role.node.id
  policy = data.aws_iam_policy_document.node.json
}

resource "aws_iam_role_policy_attachment" "ssm_core" {
  role       = aws_iam_role.node.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "node" {
  name = "via-node"
  role = aws_iam_role.node.name
}
